'use strict';

const path = require('node:path');
const { randomUUID } = require('node:crypto');
const vscode = require('vscode');

function defaultFilters() {
  return { branches: null, authors: null, refs: { local: true, remote: true, tags: true, stashes: true } };
}

function copyFilters(filters) {
  return { branches: filters.branches && [...filters.branches], authors: filters.authors && [...filters.authors],
    refs: { ...filters.refs } };
}

function selectionSummary(values) {
  return values === null ? 'All' : `${values.length} selected`;
}

function validFilePath(value) {
  return typeof value === 'string' && !!value && !path.isAbsolute(value) &&
    !value.split(/[\\/]/).some(part => !part || part === '.' || part === '..');
}

class GraphProvider {
  constructor(tree, backend, git, extensionUri) {
    this.tree = tree;
    this.backend = backend;
    this.git = git;
    this.extensionUri = extensionUri;
    this.panel = undefined;
    this.repoKey = undefined;
    this.guard = () => true;
    this.rows = new Map();
    this.displayedRows = [];
    this.segments = new Set();
    this.collapsedSegments = new Set();
    this.files = new Map();
    this.firstParent = false;
    this.filters = defaultFilters();
    this.branches = [];
    this.authors = new Set();
    this.graphGeneration = 0;
    this.detailGeneration = 0;
    this.selected = undefined;
    this.lastHead = undefined;
    this.revision = undefined;
    this.pollTimer = undefined;
    this.pollPending = false;
    this.pollAgain = false;
    this.pollEpoch = 0;
    this.ready = false;
    this.loading = false;
    this.unstableLoads = 0;
    this.retryBackoff = false;
    this.subscription = tree.onDidChangeTreeData(() => {
      if (!this.panel) return;
      const repo = this.currentRepo();
      if (!repo) return this.invalidate('Repository is no longer in the workspace.');
      const head = repo.state?.HEAD?.commit;
      if (head !== this.lastHead) {
        this.lastHead = head;
        if (this.panel.visible === false) this.revision = undefined;
        else if (this.loading || this.pollPending) this.pollAgain = true;
        else if (this.retryBackoff || this.unstableLoads > 2) { if (!this.pollTimer) this.schedulePoll(); }
        else this.schedulePoll(0);
      } else if (this.revision === undefined && !this.loading && !this.pollPending) {
        if (this.retryBackoff || this.unstableLoads > 2) { if (!this.pollTimer) this.schedulePoll(); }
        else this.schedulePoll(0);
      }
    });
  }

  currentRepo() {
    const repo = this.tree.git.repositories.find(item => item.rootUri.toString() === this.repoKey);
    return this.guard() && repo && this.tree.isVisible(repo) ? repo : undefined;
  }

  open(repo, guard = () => true) {
    const token = repo && this.tree.repositoryToken(repo);
    const currentGuard = () => guard() && this.tree.isCurrentRepository(repo, token);
    if (!repo || !currentGuard() || !this.tree.git.repositories.some(item => item.rootUri.toString() === repo.rootUri.toString())) {
      throw new Error('Repository is no longer in the workspace.');
    }
    this.repoKey = repo.rootUri.toString();
    this.guard = currentGuard;
    this.lastHead = repo.state?.HEAD?.commit;
    this.stopPolling();
    this.ready = false;
    this.revision = undefined;
    this.unstableLoads = 0;
    this.retryBackoff = false;
    this.firstParent = false;
    this.filters = defaultFilters();
    this.branches = [];
    this.authors.clear();
    this.rows.clear();
    this.displayedRows = [];
    this.segments.clear();
    this.collapsedSegments.clear();
    this.files.clear();
    this.selected = undefined;
    this.graphGeneration++;
    this.detailGeneration++;
    if (this.panel) {
      this.panel.title = `Gitpane: ${path.basename(repo.rootUri.fsPath)}`;
      this.panel.reveal();
      this.ready = true;
      this.loadGraph();
      return;
    }
    const panel = vscode.window.createWebviewPanel('gitpane.graph',
      `Gitpane: ${path.basename(repo.rootUri.fsPath)}`, vscode.ViewColumn.Active, {
        enableScripts: true,
        localResourceRoots: [vscode.Uri.joinPath(this.extensionUri, 'src', 'graph')]
      });
    this.panel = panel;
    panel.webview.html = require('./graph/html').getHtml(panel.webview, this.extensionUri);
    panel.webview.onDidReceiveMessage(message => this.receive(message));
    panel.onDidChangeViewState?.(() => {
      if (this.panel !== panel || !this.ready) return;
      if (panel.visible === false) {
        this.stopPolling();
        this.revision = undefined;
        this.graphGeneration++;
        this.detailGeneration++;
        this.files.clear();
        this.loading = false;
      } else this.schedulePoll(0);
    });
    panel.onDidDispose(() => {
      if (this.panel !== panel) return;
      this.panel = undefined;
      this.stopPolling();
      this.ready = false;
      this.repoKey = undefined;
      this.guard = () => true;
      this.graphGeneration++;
      this.detailGeneration++;
      this.rows.clear();
      this.segments.clear();
      this.collapsedSegments.clear();
      this.files.clear();
    });
  }

  post(message) {
    if (this.panel) this.panel.webview.postMessage(message);
  }

  stopPolling() {
    this.pollEpoch++;
    if (this.pollTimer) clearTimeout(this.pollTimer);
    this.pollTimer = undefined;
    this.pollAgain = false;
  }

  schedulePoll(delay = 5000) {
    if (!this.ready || !this.panel || this.panel.visible === false) return;
    if (this.pollTimer) clearTimeout(this.pollTimer);
    this.pollTimer = setTimeout(() => { this.pollTimer = undefined; void this.pollRevision(); }, delay);
    this.pollTimer.unref?.();
  }

  async readRevision(repo) {
    const result = await this.backend.graphRevision(repo.rootUri.fsPath);
    if (!result || typeof result.revision !== 'string' || !/^[a-f0-9]{64}$/.test(result.revision)) {
      throw new Error('Gitpane returned an invalid graph revision.');
    }
    return result.revision;
  }

  async pollRevision() {
    if (!this.ready || !this.panel || this.panel.visible === false) return;
    if (this.pollPending || this.loading) { this.pollAgain = true; return; }
    const repo = this.currentRepo();
    if (!repo) {
      this.invalidate('Repository is no longer in the workspace.');
      this.schedulePoll();
      return;
    }
    const panel = this.panel;
    const epoch = this.pollEpoch;
    const generation = this.graphGeneration;
    const current = () => this.panel === panel && panel.visible !== false && this.pollEpoch === epoch &&
      this.graphGeneration === generation && this.currentRepo() === repo;
    this.pollPending = true;
    try {
      const revision = await this.readRevision(repo);
      if (current()) {
        this.retryBackoff = false;
        if (revision !== this.revision) await this.loadGraph();
      }
    } catch (error) {
      if (current()) {
        this.revision = undefined;
        this.retryBackoff = true;
        this.invalidate(error.message, true);
      }
    } finally {
      this.pollPending = false;
      if (this.ready && this.panel && this.panel.visible !== false) {
        const delay = this.pollAgain ? 0 : 5000;
        this.pollAgain = false;
        this.schedulePoll(delay);
      }
    }
  }

  invalidate(message, preserve = false) {
    const previousRows = preserve ? this.displayedRows : [];
    this.graphGeneration++;
    this.loading = false;
    this.revision = undefined;
    this.pollAgain = false;
    this.detailGeneration++;
    this.rows.clear();
    this.segments.clear();
    if (!preserve) this.collapsedSegments.clear();
    if (!preserve) this.displayedRows = [];
    if (!preserve) this.retryBackoff = false;
    this.files.clear();
    if (!preserve) this.selected = undefined;
    this.post({ type: 'graph', rows: previousRows, loading: false, error: message, firstParent: this.firstParent,
      filters: copyFilters(this.filters) });
  }

  async loadGraph() {
    const repo = this.currentRepo();
    if (!repo) {
      this.invalidate('Repository is no longer in the workspace.');
      return;
    }
    if (!this.panel || this.panel.visible === false) {
      this.revision = undefined;
      return;
    }
    const generation = ++this.graphGeneration;
    const panel = this.panel;
    const previousRows = this.displayedRows;
    const previousSelection = this.selected;
    this.loading = true;
    this.detailGeneration++;
    this.rows.clear();
    this.segments.clear();
    this.files.clear();
    const repoName = path.basename(repo.rootUri.fsPath);
    this.post({ type: 'graph', repoName, rows: previousRows, loading: true, firstParent: this.firstParent,
      filters: copyFilters(this.filters), collapsedCount: this.collapsedSegments.size });
    try {
      const before = await this.readRevision(repo);
      if (generation !== this.graphGeneration || this.panel !== panel || this.currentRepo() !== repo || panel.visible === false) return;
      const result = await this.backend.graph(repo.rootUri.fsPath, this.firstParent, copyFilters(this.filters), [...this.collapsedSegments]);
      if (generation !== this.graphGeneration || this.panel !== panel || this.currentRepo() !== repo || panel.visible === false) return;
      const after = await this.readRevision(repo);
      if (generation !== this.graphGeneration || this.panel !== panel || this.currentRepo() !== repo || panel.visible === false) return;
      if (before !== after) {
        this.revision = undefined;
        this.unstableLoads++;
        this.pollAgain = this.unstableLoads <= 2;
        return;
      }
      if (!Array.isArray(result?.rows) || !Array.isArray(result?.branches) ||
        result.branches.some(branch => typeof branch !== 'string')) throw new Error('Gitpane returned an invalid graph.');
      const labelMaxLen = result.label_max_len === undefined ? 24 : result.label_max_len;
      if (!Number.isSafeInteger(labelMaxLen) || labelMaxLen < 0) {
        throw new Error('Gitpane returned an invalid graph label length.');
      }
      this.rows = new Map(result.rows.filter(row => typeof row?.oid === 'string' && /^[a-f0-9]{40,64}$/.test(row.oid)).map(row => [row.oid, row]));
      if (this.rows.size !== result.rows.length) throw new Error('Gitpane returned an invalid graph commit.');
      const segments = new Set();
      const collapsed = new Set();
      for (const row of result.rows) {
        if (row.segment_id !== undefined && row.segment_id !== null) {
          if (typeof row.segment_id !== 'string' || !/^[a-f0-9]{40,64}$/.test(row.segment_id)) throw new Error('Gitpane returned an invalid graph segment.');
          segments.add(row.segment_id);
        }
        if (row.collapsed !== undefined && row.collapsed !== null) {
          if (!row.collapsed || !segments.has(row.collapsed.id) || row.collapsed.id !== row.segment_id ||
            !Number.isSafeInteger(row.collapsed.count) || row.collapsed.count < 1) {
            throw new Error('Gitpane returned an invalid collapsed branch.');
          }
          collapsed.add(row.collapsed.id);
        }
      }
      this.segments = segments;
      this.collapsedSegments = collapsed;
      this.branches = [...new Set(result.branches)].sort();
      for (const row of result.rows) if (typeof row.author === 'string' && row.author) this.authors.add(row.author);
      this.revision = after;
      this.unstableLoads = 0;
      this.retryBackoff = false;
      this.displayedRows = result.rows;
      this.selected = previousSelection && this.rows.has(previousSelection) && !this.rows.get(previousSelection).collapsed ? previousSelection : undefined;
      this.post({ type: 'graph', repoName, rows: result.rows, limit: result.limit, loading: false,
        labelMaxLen, firstParent: this.firstParent, filters: copyFilters(this.filters), collapsedCount: collapsed.size });
      if (this.selected) await this.loadCommit(this.selected);
    } catch (error) {
      if (generation !== this.graphGeneration || this.panel !== panel || panel.visible === false || this.currentRepo() !== repo) return;
      this.rows.clear();
      this.segments.clear();
      this.revision = undefined;
      this.retryBackoff = true;
      this.selected = previousSelection;
      this.post({ type: 'graph', repoName, rows: previousRows, loading: false, error: error.message,
        firstParent: this.firstParent, filters: copyFilters(this.filters) });
    } finally {
      if (generation === this.graphGeneration) {
        this.loading = false;
        if (!this.pollPending) {
          const delay = this.pollAgain ? 0 : 5000;
          this.pollAgain = false;
          this.schedulePoll(delay);
        }
      }
    }
  }

  async openFilters() {
    const repo = this.currentRepo();
    if (!repo || !this.panel) return;
    const generation = this.graphGeneration;
    const panel = this.panel;
    const stillCurrent = () => this.panel === panel && this.currentRepo()?.rootUri.toString() === repo.rootUri.toString() &&
      this.graphGeneration === generation;
    const hiddenRefs = Object.values(this.filters.refs).filter(value => !value).length;
    const categories = [
      { label: 'Branches', description: selectionSummary(this.filters.branches), category: 'branches' },
      { label: 'Authors', description: selectionSummary(this.filters.authors), category: 'authors' },
      { label: 'References', description: hiddenRefs ? `${hiddenRefs} hidden` : 'All', category: 'refs' },
      { label: 'Reset Filters', category: 'reset' }
    ];
    const category = await vscode.window.showQuickPick(categories, { placeHolder: 'Choose a graph filter' });
    if (!category || !categories.includes(category) || !stillCurrent()) return;
    if (category.category === 'reset') {
      this.filters = defaultFilters();
      this.collapsedSegments.clear();
      await this.loadGraph();
      return;
    }
    let choices;
    if (category.category === 'refs') {
      choices = [
        ['local', 'Local branches'], ['remote', 'Remote branches'], ['tags', 'Tags'], ['stashes', 'Stashes']
      ].map(([value, label]) => ({ value, label, picked: this.filters.refs[value] }));
    } else {
      const selected = this.filters[category.category];
      const catalog = category.category === 'branches' ? this.branches : [...this.authors];
      const values = [...new Set([...catalog, ...(selected || [])])].sort();
      if (!values.length) {
        await vscode.window.showInformationMessage(`No ${category.category} available in the current graph.`);
        return;
      }
      choices = values.map(value => ({ value, label: value, picked: selected === null || selected.includes(value) }));
    }
    const picked = await vscode.window.showQuickPick(choices, { canPickMany: true, placeHolder: `Select ${category.label.toLowerCase()}` });
    if (!picked || !Array.isArray(picked) || picked.some(item => !choices.includes(item)) || !stillCurrent()) return;
    if (category.category === 'refs') {
      const values = new Set(picked.map(item => item.value));
      this.filters.refs = { local: values.has('local'), remote: values.has('remote'),
        tags: values.has('tags'), stashes: values.has('stashes') };
    } else {
      this.filters[category.category] = picked.length === choices.length ? null : picked.map(item => item.value);
    }
    this.collapsedSegments.clear();
    await this.loadGraph();
  }

  async loadCommit(oid) {
    const repo = this.currentRepo();
    const row = this.rows.get(oid);
    if (!repo || !row || row.collapsed) return;
    const generation = ++this.detailGeneration;
    this.selected = oid;
    this.files.clear();
    this.post({ type: 'commitFiles', oid, files: [], loading: true });
    try {
      const result = await this.backend.commitFiles(repo.rootUri.fsPath, oid);
      if (generation !== this.detailGeneration || !this.currentRepo() || !this.panel) return;
      if (!Array.isArray(result?.files)) throw new Error('Gitpane returned invalid commit files.');
      const files = result.files.map(file => {
        if (!file || !validFilePath(file.path) || typeof file.status !== 'string' ||
          (file.status === 'R' && !validFilePath(file.old_path))) {
          throw new Error('Gitpane returned an invalid commit path.');
        }
        const id = randomUUID();
        this.files.set(id, { path: file.path, oldPath: file.old_path, status: file.status, isSubmodule: file.is_submodule, oid, parent: row.parent_oids?.[0] });
        return { id, path: file.path, status: file.status };
      });
      this.post({ type: 'commitFiles', oid, files, loading: false });
    } catch (error) {
      if (generation !== this.detailGeneration || !this.panel) return;
      this.files.clear();
      this.post({ type: 'commitFiles', oid, files: [], loading: false, error: error.message });
    }
  }

  async openCommitFile(id) {
    const repo = this.currentRepo();
    const file = this.files.get(id);
    if (!repo || !file || file.oid !== this.selected) return;
    if (file.isSubmodule) throw new Error('Submodule changes cannot be opened as a file diff yet.');
    const uri = vscode.Uri.file(path.join(repo.rootUri.fsPath, file.path));
    const oldUri = file.status === 'R' ? vscode.Uri.file(path.join(repo.rootUri.fsPath, file.oldPath)) : uri;
    const empty = uri.with({ scheme: 'gitpane-empty', query: '' });
    const left = file.status === 'A' || !file.parent ? empty : this.git.toGitUri(oldUri, file.parent);
    const right = file.status === 'D' ? empty : this.git.toGitUri(uri, file.oid);
    await vscode.commands.executeCommand('vscode.diff', left, right, `${path.basename(file.path)} (${file.oid.slice(0, 8)})`);
  }

  async receive(message) {
    if (!message || typeof message.type !== 'string') return;
    if (message.type === 'ready') { this.ready = true; return this.loadGraph(); }
    if (message.type === 'refresh') return this.loadGraph();
    if (message.type === 'openFilters') {
      try { return await this.openFilters(); }
      catch (error) { return vscode.window.showErrorMessage(`Gitpane could not filter the graph: ${error.message}`); }
    }
    if (message.type === 'firstParent' && typeof message.value === 'boolean') {
      if (this.firstParent !== message.value) this.collapsedSegments.clear();
      this.firstParent = message.value;
      return this.loadGraph();
    }
    if (message.type === 'toggleCollapse' && typeof message.id === 'string') {
      if (!this.currentRepo() || !this.panel || !this.segments.has(message.id)) return;
      if (this.collapsedSegments.has(message.id)) this.collapsedSegments.delete(message.id);
      else this.collapsedSegments.add(message.id);
      return this.loadGraph();
    }
    if (message.type === 'expandAll') {
      if (!this.currentRepo() || !this.panel || !this.collapsedSegments.size) return;
      this.collapsedSegments.clear();
      return this.loadGraph();
    }
    if (message.type === 'selectCommit' && typeof message.oid === 'string') return this.loadCommit(message.oid);
    if (message.type === 'openCommitFile' && typeof message.id === 'string') {
      try { await this.openCommitFile(message.id); }
      catch (error) { vscode.window.showErrorMessage(`Gitpane could not open the diff: ${error.message}`); }
    }
  }

  dispose() {
    this.ready = false;
    this.stopPolling();
    this.subscription.dispose();
    this.panel?.dispose();
    this.panel = undefined;
    this.rows.clear();
    this.segments.clear();
    this.collapsedSegments.clear();
    this.files.clear();
  }
}

module.exports = { GraphProvider };
