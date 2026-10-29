'use strict';

const vscode = require('vscode');
const fs = require('node:fs');
const path = require('node:path');
const { randomUUID } = require('node:crypto');
const { BackendManager } = require('./backend-manager');
const { GraphProvider } = require('./graph-provider');
const { BranchReviewProvider } = require('./branch-review-provider');
const { GithubProvider } = require('./github-provider');
const { SubmoduleProvider } = require('./submodule-provider');
const { SessionProvider } = require('./session-provider');
const { RepoTerminalProvider } = require('./repo-terminal-provider');
const { LauncherProvider } = require('./launcher-provider');
const { MutationManager } = require('./mutation-manager');
const { SidebarActions } = require('./sidebar-actions');
const { Status, isSubmodule, richComparison, statusLabel } = require('./model');
const { RepositoryTree } = require('./repository-tree');
const { ChangePager, scopedFile, validRecord, validatePage } = require('./change-pager');
const { LinkDocuments, statusRevision } = require('./link-documents');
const { ThemeController } = require('./theme');
const { WorkspaceFetch } = require('./workspace-fetch');
const { SubmodulePins } = require('./submodule-pins');

const RICH_STATUS = {
  index_modified: Status.INDEX_MODIFIED, index_added: Status.INDEX_ADDED, index_deleted: Status.INDEX_DELETED,
  index_renamed: Status.INDEX_RENAMED, index_copied: Status.INDEX_COPIED, index_type_changed: Status.TYPE_CHANGED,
  working_modified: Status.MODIFIED, working_deleted: Status.DELETED, working_renamed: Status.INTENT_TO_RENAME,
  intent_to_add: Status.INTENT_TO_ADD, type_changed: Status.TYPE_CHANGED, untracked: Status.UNTRACKED,
  untracked_directory: Status.UNTRACKED,
  conflicted: Status.BOTH_MODIFIED
};

class GitpaneViewProvider {
  constructor(tree, extensionUri, theme) {
    this.tree = tree;
    this.theme = theme;
    this.themeSubscription = theme?.onDidChange(() => this.publish());
    this.extensionUri = extensionUri;
    this.view = undefined;
    this.ready = false;
    this.sessionId = randomUUID();
    this.nextId = 0;
    this.ids = new Map();
    this.repositories = new Map();
    this.changes = new Map();
    this.worktrees = new Map();
    this.viewDisposables = [];
    this.pager = new ChangePager(tree.backend, () => this.publish(), error => tree.output?.appendLine(`Could not release changes: ${error.message}`));
    this.refreshChangesTimer = undefined;
    this.refreshChangesInFlight = undefined;
    this.subscription = tree.onDidChangeTreeData(() => { this.publish(); this.scheduleChangesRefresh(); });
  }

  idFor(key, live) {
    live.add(key);
    if (!this.ids.has(key)) this.ids.set(key, `item-${this.sessionId}-${++this.nextId}`);
    return this.ids.get(key);
  }

  snapshot() {
    const live = new Set();
    const changes = new Map();
    const worktrees = new Map();
    const repositoriesById = new Map();
    const groupsFor = (repo, ownerId, parentWorktree) => {
      const root = repo.rootUri.fsPath;
      const repoKey = repo.rootUri.toString();
      const repoToken = this.tree.repositoryToken(repo);
      const state = this.pager.get(ownerId);
      const labels = { merge: 'Conflicts', index: 'Staged', working: 'Unstaged', untracked: 'Untracked',
        attention: 'Submodules needing attention' };
      const grouped = new Map();
      for (const record of state?.changes || []) {
        const uri = vscode.Uri.file(path.join(root, ...record.path.split('/')));
        const change = { uri, status: RICH_STATUS[record.kind] };
        const fileId = this.idFor(JSON.stringify(['file', ownerId, repoKey, repoToken, record.group, record.path]), live);
        const directory = record.kind === 'untracked_directory';
        if (!directory) changes.set(fileId, { type: 'change', repo, repoToken, group: record.group, change, fileId,
          parentWorktree, ownerId, record });
        if (!grouped.has(record.group)) grouped.set(record.group, { id: record.group, label: labels[record.group], files: [] });
        grouped.get(record.group).files.push({ id: fileId, name: path.basename(uri.fsPath),
          path: directory ? `${record.path}/` : record.path, status: record.group === 'attention' ? '!' : statusLabel(change.status),
          ...(directory ? { directory: true } : {}), ...(record.is_submodule ? { submodule: true } : {}) });
      }
      return ['merge', 'index', 'working', 'untracked', 'attention'].flatMap(group => grouped.has(group) ? [grouped.get(group)] : []);
    };
    const pageFor = id => {
      const state = this.pager.get(id);
      return { state: state?.phase || 'idle', page: state?.page || 0,
        previous: state?.previous_cursor != null, next: state?.next_cursor != null,
        ...(state?.error ? { error: state.error } : {}) };
    };
    const lowerBound = status => status?.file_count_lower_bound ?? 0;
    const hierarchy = this.tree.workspaceView?.();
    const children = this.tree.getChildren(undefined, hierarchy);
    const repositories = children.filter(node => node.type === 'repo').map(node => {
      const repo = node.repo;
      const root = repo.rootUri.fsPath;
      const repoKey = repo.rootUri.toString();
      const repoToken = this.tree.repositoryToken(repo);
      const status = this.tree.backendStatuses.get(repoKey);
      const id = this.idFor(JSON.stringify(['repo', repoKey, repoToken]), live);
      repositoriesById.set(id, { repoKey, repoToken });
      const head = repo.state.HEAD;
      const stashes = (status?.stashes || []).filter(item => item && Number.isSafeInteger(item.index) &&
        item.index >= 0 && typeof item.message === 'string' && typeof item.oid === 'string').map(item => ({
        id: this.idFor(JSON.stringify(['stash', repoKey, repoToken, item.oid, item.index]), live),
        index: item.index, message: item.message
      }));
      const worktreeData = this.tree.workspaceWorktrees(repo, hierarchy).map(item => {
        const child = this.tree.worktreeRepository(repo, item.path, hierarchy);
        const childRepoKey = child?.rootUri.toString();
        const childStatus = child && this.tree.backendStatuses.get(childRepoKey);
        const childToken = child && this.tree.repositoryToken(child);
        const worktreeId = this.idFor(JSON.stringify(['worktree', repoKey, repoToken, item.path, childToken]), live);
        const childRepoId = childRepoKey && this.idFor(JSON.stringify(['repo', childRepoKey, childToken]), live);
        if (childRepoId) repositoriesById.set(childRepoId, { repoKey: childRepoKey, repoToken: childToken });
        worktrees.set(worktreeId, { repoKey, repoToken, path: item.path, childRepoId, childToken });
        return { id: worktreeId, name: path.basename(item.path), branch: item.branch, live: this.tree.sessionLive?.(item.path) || false,
          dirty: item.is_dirty, fileCount: item.file_count, ahead: item.ahead, behind: item.behind,
          available: !!child, hasUnpushedSubmodules: !!childStatus?.has_unpushed_submodules,
          changeCountLowerBound: child ? lowerBound(childStatus) : 0,
          changesPage: pageFor(worktreeId), groups: child ? groupsFor(child, worktreeId,
            { repoKey, repoToken, path: item.path }) : [] };
      });
      const groupData = groupsFor(repo, id);
      return {
        id,
        name: node.duplicate ? `${path.basename(root)} (${path.dirname(root)})` : path.basename(root),
        branch: (status?.branch === '(no branch)' && !status.head_oid && head?.name) ||
          status?.branch || head?.name || (head?.commit ? head.commit.slice(0, 8) : 'unborn'),
        dirty: !!status?.is_dirty || this.tree.groupsFor(repo).some(group => group.changes.length),
        ahead: status?.ahead || 0,
        behind: status?.behind || 0,
        worktree: repo.kind === 'worktree',
        pinned: this.pins?.isPinned(repo) || false,
        live: this.tree.sessionLive?.(root) || false,
        statusUnavailable: !status,
        stashCount: status?.stashes.length || 0,
        stashes,
        worktreeCount: worktreeData.length,
        worktrees: worktreeData,
        hasDirtySubmodules: status?.has_dirty_submodules || false,
        hasUnpushedSubmodules: status?.has_unpushed_submodules || false,
        changeCountLowerBound: lowerBound(status), changesPage: pageFor(id), groups: groupData
      };
    });
    for (const key of this.ids.keys()) if (!live.has(key)) this.ids.delete(key);
    this.changes = changes;
    this.worktrees = worktrees;
    this.repositories = repositoriesById;
    return {
      type: 'snapshot', repositories,
      errors: [...children.filter(node => node.type === 'error').map(node => node.message), ...(this.theme?.error ? [this.theme.error] : [])],
      ...(this.theme?.value ? { theme: this.theme.payload() } : {}),
      ...(this.theme && !this.theme.value ? { themeClear: true } : {}),
      emptyMessage: children.find(node => node.type === 'info')?.message,
      loading: this.tree.busy
    };
  }

  publish() {
    if (!this.view || !this.ready) return;
    this.view.webview.postMessage(this.snapshot());
  }

  currentOwner(id) {
    const repository = this.currentRepository(id);
    if (repository) return repository;
    return this.currentWorktreeRepository(id);
  }

  async loadChanges(id) {
    const repo = this.currentOwner(id);
    if (!repo) return;
    const token = this.tree.repositoryToken(repo);
    const guard = () => this.currentOwner(id) === repo && this.tree.isCurrentRepository(repo, token);
    await this.pager.refresh(id, repo.rootUri.fsPath, guard);
    this.scheduleChangesRefresh();
  }

  scheduleChangesRefresh() {
    if (this.refreshChangesTimer !== undefined || this.refreshChangesInFlight || !this.view?.visible || !this.pager.entries.size) return;
    this.refreshChangesTimer = setTimeout(async () => {
      this.refreshChangesTimer = undefined;
      if (!this.view?.visible) return;
      const sweep = (async () => {
        for (const id of [...this.pager.entries.keys()]) {
          if (!this.view?.visible) return;
          if (!this.currentOwner(id)) { this.pager.release(id); continue; }
          await this.loadChanges(id);
        }
      })();
      this.refreshChangesInFlight = sweep;
      try { await sweep; }
      catch (error) {
        this.tree.fail(`Could not refresh changes: ${error.message}`);
        this.tree.changed();
      } finally {
        if (this.refreshChangesInFlight === sweep) this.refreshChangesInFlight = undefined;
        this.scheduleChangesRefresh();
      }
    }, 5000);
  }

  async revalidateChange(id) {
    const selected = this.changes.get(id);
    const node = this.currentChange(selected);
    if (!node) return false;
    const result = await this.tree.backend.verifyChanges(node.repo.rootUri.fsPath, node.record);
    validatePage(result?.page, undefined, 0);
    if (!this.currentChange(node)) return false;
    if (result.change !== null && !validRecord(result.change)) throw new Error('Gitpane returned an invalid file check.');
    if (JSON.stringify(result.change) !== JSON.stringify(node.record)) {
      await this.loadChanges(node.ownerId);
      return false;
    }
    return true;
  }

  async waitForCurrentChange(node) {
    const state = this.pager.get(node?.ownerId);
    if (state?.pending) await state.pending;
    return this.currentChange(node);
  }

  async refresh(retryBackend = false) {
    await this.tree.refresh(retryBackend);
    if (retryBackend && this.theme?.error) await this.theme.retry();
    for (const id of [...this.pager.entries.keys()]) {
      const pending = this.pager.get(id)?.pending;
      if (pending) await pending;
      if (this.currentOwner(id)) await this.loadChanges(id);
      else this.pager.release(id);
    }
  }

  currentChange(node) {
    const stored = this.changes.get(node?.fileId);
    if (!stored || stored.repo !== node.repo || stored.repoToken !== node.repoToken || stored.group !== node.group ||
        stored.ownerId !== node.ownerId || stored.change.uri.toString() !== node.change?.uri?.toString() ||
        JSON.stringify(stored.record) !== JSON.stringify(node.record)) return undefined;
    node = stored;
    if (!node?.repo?.rootUri || !node.change?.uri || !this.tree.isCurrentRepository(node.repo, node.repoToken)) return undefined;
    const repoKey = node.repo.rootUri.toString();
    const repo = this.tree.git.repositories.find(item => item.rootUri.toString() === repoKey);
    if (!repo) return undefined;
    if (node.parentWorktree) {
      const parent = this.tree.git.repositories.find(item => item.rootUri.toString() === node.parentWorktree.repoKey);
      const worktreeRepo = parent && this.tree.isCurrentRepository(parent, node.parentWorktree.repoToken) &&
        this.tree.workspaceWorktrees(parent).some(item => item.path === node.parentWorktree.path) &&
        this.tree.worktreeRepository(parent, node.parentWorktree.path);
      if (!worktreeRepo || worktreeRepo.rootUri.toString() !== repoKey) return undefined;
    }
    const state = this.pager.get(node.ownerId);
    if (state?.phase !== 'ready' || !state.guard()) return undefined;
    const record = state.changes.find(item => item.group === node.group && item.path === node.record?.path);
    if (!record || JSON.stringify(record) !== JSON.stringify(node.record)) return undefined;
    return { ...node, repo, record };
  }

  currentRepository(id) {
    const selected = this.repositories.get(id);
    if (!selected) return undefined;
    const { repoKey, repoToken } = selected;
    const repo = this.tree.git.repositories.find(item => item.rootUri.toString() === repoKey);
    return repo && this.tree.isCurrentRepository(repo, repoToken) ? repo : undefined;
  }

  actionRepository(repo) {
    if (!repo || !this.tree.isVisible(repo)) return undefined;
    const status = this.tree.backendStatuses.get(repo.rootUri.toString());
    if (!status || typeof status.common_dir !== 'string' || !path.isAbsolute(status.common_dir)) return undefined;
    return { repoPath: repo.rootUri.fsPath, commonDir: status.common_dir,
      branch: status.branch === 'HEAD' ? null : status.branch, head: status.head_oid };
  }

  resolveRepository(id) {
    const repo = this.currentRepository(id);
    const actionRepo = this.actionRepository(repo);
    if (!actionRepo) return undefined;
    actionRepo.pinned = this.pins?.isPinned(repo) || false;
    const found = this.tree.explicitWorktreeAnchor?.(repo);
    if (!found || !this.tree.isPathInWorkspace(found.path)) return actionRepo;
    const anchorToken = this.tree.repositoryToken(found.anchor);
    const targetToken = this.tree.repositoryToken(repo);
    const anchor = this.actionRepository(found.anchor);
    if (!anchor || !this.tree.isCurrentRepository(found.anchor, anchorToken) || !this.tree.isCurrentRepository(repo, targetToken)) return actionRepo;
    return { ...actionRepo, worktreeRemoval: { ...anchor, path: found.path, branch: found.branch,
      anchorKey: found.anchor.rootUri.toString(), anchorToken, targetToken, commonIdentity: found.commonIdentity } };
  }

  resolveChange(id) {
    const node = this.currentChange(this.changes.get(id));
    const repo = node && this.actionRepository(node.repo);
    if (!repo) return undefined;
    const relative = path.relative(repo.repoPath, node.change.uri.fsPath);
    if (!relative || path.isAbsolute(relative) || relative === '..' || relative.startsWith(`..${path.sep}`)) return undefined;
    return { ...repo, path: relative, group: node.group, status: node.change.status, uri: node.change.uri, node,
      recordSignature: JSON.stringify(node.record),
      isSubmodule: node.record.is_submodule || isSubmodule(repo.repoPath, node.change.uri.fsPath, node.repo.state.submodules) };
  }

  resolveWorktree(id) {
    const selected = this.worktrees.get(id);
    if (!selected) return undefined;
    const repo = this.tree.git.repositories.find(item => item.rootUri.toString() === selected.repoKey);
    if (!repo || !this.tree.isCurrentRepository(repo, selected.repoToken)) return undefined;
    const actionRepo = this.actionRepository(repo);
    if (!actionRepo || !this.tree.isPathInWorkspace(selected.path)) return undefined;
    const status = this.tree.backendStatuses.get(selected.repoKey);
    const worktree = status.worktrees.find(item => item.path === selected.path);
    if (!worktree || typeof worktree.branch !== 'string') return undefined;
    if (!this.tree.workspaceWorktrees(repo).some(item => item.path === selected.path)) return undefined;
    const child = this.tree.worktreeRepository(repo, worktree.path);
    const available = !!child && typeof selected.childRepoId === 'string' && this.tree.isCurrentRepository(child, selected.childToken);
    if (selected.childRepoId && !available) return undefined;
    return { ...actionRepo, path: worktree.path, branch: worktree.branch,
      childRepoKey: child?.rootUri.toString(), childRepoId: available ? selected.childRepoId : undefined, available };
  }

  currentWorktreeRepository(id) {
    const selected = this.worktrees.get(id);
    if (!selected?.childRepoId) return undefined;
    const parent = this.tree.git.repositories.find(repo => repo.rootUri.toString() === selected.repoKey);
    if (!parent || !this.tree.isCurrentRepository(parent, selected.repoToken) ||
        !this.tree.workspaceWorktrees(parent).some(item => item.path === selected.path)) return undefined;
    const child = this.tree.worktreeRepository(parent, selected.path);
    return child && this.tree.isCurrentRepository(child, selected.childToken) ? child : undefined;
  }

  resolveWebviewView(view) {
    for (const disposable of this.viewDisposables) disposable.dispose();
    this.viewDisposables = [];
    this.view = view;
    this.ready = false;
    view.webview.options = {
      enableScripts: true,
      localResourceRoots: [vscode.Uri.joinPath(this.extensionUri, 'src')]
    };
    const { getHtml } = require('./webview/html');
    view.webview.html = getHtml(view.webview, this.extensionUri);
    this.tree.setVisible(view.visible);
    this.viewDisposables.push(
      view.webview.onDidReceiveMessage(async message => {
        if (message?.type === 'ready') {
          this.ready = true;
          this.publish();
        } else if (message?.type === 'refresh') {
          try { await this.refresh(true); }
          catch (error) {
            this.tree.fail(`Could not refresh repository status: ${error.message}`);
            this.tree.changed();
          }
        } else if (message?.type === 'openChange' && typeof message.id === 'string') {
          const selected = this.changes.get(message.id);
          const node = selected && await this.waitForCurrentChange(selected);
          if (node) await vscode.commands.executeCommand('gitpane.openChange', node);
          else if (selected) await vscode.window.showErrorMessage('Gitpane change is no longer available. Refresh and try again.');
        } else if (message?.type === 'loadChanges' && typeof message.id === 'string') {
          await this.loadChanges(message.id);
        } else if (message?.type === 'changesPage' && typeof message.id === 'string') {
          await this.pager.move(message.id, message.direction);
        } else if (message?.type === 'releaseChanges' && typeof message.id === 'string') {
          this.pager.release(message.id);
          this.publish();
        } else if (message?.type === 'openGraph' && typeof message.id === 'string') {
          if (this.currentRepository(message.id)) await vscode.commands.executeCommand('gitpane.openGraph', message.id);
        } else if (message?.type === 'openWorktreeGraph' && typeof message.id === 'string') {
          await this.openWorktreeGraph?.(message.id);
        } else if (message?.type === 'menu' && typeof message.id === 'string') {
          if (message.kind === 'file') {
            const selected = this.changes.get(message.id);
            if (selected && !await this.waitForCurrentChange(selected)) {
              await vscode.window.showErrorMessage('Gitpane change is no longer available. Refresh and try again.');
              return;
            }
          }
          await this.actions?.showMenu(message.kind, message.id);
        }
      }),
      view.onDidChangeVisibility(() => {
        this.tree.setVisible(view.visible);
        if (!view.visible) {
          if (this.refreshChangesTimer !== undefined) clearTimeout(this.refreshChangesTimer);
          this.refreshChangesTimer = undefined;
          for (const id of this.pager.entries.keys()) this.pager.release(id);
        } else {
          this.publish();
          this.scheduleChangesRefresh();
        }
      }),
      view.onDidDispose(() => {
        if (this.view !== view) return;
        this.tree.setVisible(false);
        for (const id of this.pager.entries.keys()) this.pager.release(id);
        this.view = undefined;
        this.ready = false;
        this.repositories.clear();
        this.changes.clear();
        this.worktrees.clear();
      })
    );
  }

  dispose() {
    this.themeSubscription?.dispose();
    this.tree.setVisible(false);
    if (this.refreshChangesTimer !== undefined) clearTimeout(this.refreshChangesTimer);
    this.pager.dispose();
    this.subscription.dispose();
    for (const disposable of this.viewDisposables) disposable.dispose();
    this.viewDisposables = [];
    this.view = undefined;
    this.repositories.clear();
    this.changes.clear();
    this.worktrees.clear();
  }
}

async function activate(context) {
  const output = vscode.window.createOutputChannel('Gitpane');
  context.subscriptions.push(output);
  const gitExtension = vscode.extensions.getExtension('vscode.git');
  if (!gitExtension) {
    vscode.window.showErrorMessage('Gitpane requires the built-in Git extension.');
    return;
  }
  const git = await gitExtension.activate();
  if (!git.enabled) {
    vscode.window.showErrorMessage('Gitpane requires the built-in Git extension to be enabled.');
    return;
  }
  const api = git.getAPI(1);
  const backend = new BackendManager(context, { output });
  const theme = new ThemeController(backend, vscode.window.onDidChangeActiveColorTheme);
  const tree = new RepositoryTree(api, output, backend, context.workspaceState?.get('gitpane.sortOrder'));
  const provider = new GitpaneViewProvider(tree, context.extensionUri, theme);
  const pins = new SubmodulePins(tree, backend, api, context.workspaceState, vscode);
  tree.pins = pins;
  provider.pins = pins;
  const links = new LinkDocuments(vscode, tree, backend);
  tree.watchedPathsFor = repo => links.watchedPaths(repo);
  tree.watchTokenFor = (repo, relative) => links.watchTokenFor(repo, relative);
  const graph = new GraphProvider(tree, backend, api, context.extensionUri, theme);
  const review = new BranchReviewProvider(tree, backend, api);
  const github = new GithubProvider(tree, backend);
  const sessions = new SessionProvider(tree, backend);
  const terminals = new RepoTerminalProvider(tree);
  const launchers = new LauncherProvider(tree, backend);
  const submodules = new SubmoduleProvider(tree, backend, api, {
    resolveChange: node => provider.currentChange(node),
    openGraph: (repo, guard, validate) => graph.open(repo, guard, validate),
    linkDocuments: links, pins
  });
  const mutations = new MutationManager({
    getSnapshot: () => backend.mutationSnapshot(),
    isCurrent: (snapshot, repo) => backend.isCurrentMutation(snapshot, repo),
    onStderr: text => output.append(text)
  });
  const workspaceFetch = new WorkspaceFetch({ tree, backend, mutations, vscode, output,
    refresh: async () => { await provider.refresh(); await graph.pollRevision(); } });
  const openWorktreeGraph = async (id, guard = () => true) => {
    const repo = provider.currentWorktreeRepository(id);
    if (!repo) return;
    const current = () => guard() && provider.currentWorktreeRepository(id)?.rootUri.toString() === repo.rootUri.toString();
    if (!current()) return;
    try { graph.open(repo, current); }
    catch (error) { vscode.window.showErrorMessage(`Gitpane could not open the graph: ${error.message}`); }
  };
  provider.openWorktreeGraph = openWorktreeGraph;
  provider.actions = new SidebarActions({ vscode, mutations,
    pinSubmodule: async (id, guard) => {
      const node = provider.currentChange(provider.changes.get(id));
      if (node && guard()) await submodules.open(node, true);
    },
    unpinSubmodule: async (id, guard) => {
      const repo = provider.currentRepository(id);
      if (repo && guard()) await pins.unpin(repo, guard);
    },
    previewFileAction: (repo, target, operation) => backend.fileActionPreview(repo, target, operation),
    previewWorktreeRemoval: (repo, target) => backend.previewWorktreeRemoval(repo, target),
    planWorktree: (repo, branch) => backend.planWorktree(repo, branch),
    revalidateChange: id => provider.revalidateChange(id),
    validateChangePath: item => scopedFile(item.repoPath, item.path),
    resolveRepository: id => provider.resolveRepository(id),
    resolveChange: id => provider.resolveChange(id),
    resolveWorktree: id => provider.resolveWorktree(id),
    reviewBranch: async (id, guard = () => true) => {
      const repo = provider.currentRepository(id);
      if (repo && guard()) await review.open(repo, guard);
    },
    openGithub: async (id, guard = () => true) => {
      const repo = provider.currentRepository(id);
      if (repo && guard()) await github.open(repo, guard);
    },
    openSession: async (id, guard = () => true) => {
      const repo = provider.currentRepository(id);
      if (repo && guard()) await sessions.open(repo, guard);
    },
    openTerminal: async (id, guard = () => true) => {
      const repo = provider.currentRepository(id);
      if (repo && guard()) await terminals.open(repo, guard);
    },
    openLauncher: async (id, guard = () => true) => {
      const repo = provider.currentRepository(id);
      if (repo && guard()) await launchers.open(repo, guard);
    },
    openWorktreeGraph,
    reviewWorktree: async (id, guard = () => true) => {
      const repo = provider.currentWorktreeRepository(id);
      if (repo && guard()) await review.open(repo, guard);
    },
    onWorktreeCreated: async target => {
      if (!tree.isPathInWorkspace(target)) return;
      try {
        const opened = await api.openRepository(vscode.Uri.file(target));
        if (!opened) throw new Error('VS Code did not open the repository.');
      }
      catch (error) { throw new Error(`Worktree created, but VS Code could not open it: ${error.message}`); }
    },
    refresh: () => provider.refresh() });
  context.subscriptions.push(backend, theme, tree, provider, links, graph, review, github, sessions, terminals, launchers, submodules, pins, mutations,
    vscode.window.registerWebviewViewProvider('gitpane.repositories', provider),
    vscode.workspace.registerTextDocumentContentProvider('gitpane-empty', { provideTextDocumentContent: () => '' }),
    vscode.workspace.registerTextDocumentContentProvider('gitpane-link', links),
    vscode.commands.registerCommand('gitpane.refresh', () => provider.refresh(true)),
    vscode.commands.registerCommand('gitpane.fetchWorkspace', () => workspaceFetch.run()),
    vscode.commands.registerCommand('gitpane.manageSubmodulePins', async () => {
      try { await pins.manage(); }
      catch (error) { await vscode.window.showErrorMessage(`Gitpane submodule pins: ${error.message}`); }
    }),
    vscode.commands.registerCommand('gitpane.sort', async () => {
      const choices = [
        { label: 'Default (dirty, by path)', order: 'workspace' },
        { label: 'A–Z', order: 'alphabetical' },
        { label: 'Z–A', order: 'reverse' },
        { label: 'Dirty first (Gitpane)', order: 'dirty' }
      ];
      const selected = await vscode.window.showQuickPick(choices, { placeHolder: 'Sort Gitpane repositories' });
      if (!selected || !choices.includes(selected)) return;
      try {
        await context.workspaceState?.update('gitpane.sortOrder', selected.order);
        tree.setSortOrder(selected.order);
      } catch (error) { vscode.window.showErrorMessage(`Gitpane could not save the sort order: ${error.message}`); }
    }),
    vscode.commands.registerCommand('gitpane.openGraph', id => {
      try {
        const repo = provider.currentRepository(id);
        if (!repo) throw new Error('Repository is no longer in the workspace.');
        graph.open(repo, () => !!provider.currentRepository(id));
      }
      catch (error) { vscode.window.showErrorMessage(`Gitpane could not open the graph: ${error.message}`); }
    }),
    vscode.commands.registerCommand('gitpane.openChange', async selected => {
      try {
        let node = await provider.waitForCurrentChange(selected);
        if (!node) throw new Error('The change is no longer in the workspace.');
        if (!await provider.revalidateChange(node.fileId)) throw new Error('The change has changed; refresh and try again.');
        node = provider.currentChange(node);
        if (!node) throw new Error('The change is no longer in the workspace.');
        if (node.record.is_submodule || isSubmodule(node.repo.rootUri.fsPath, node.change.uri.fsPath, node.repo.state.submodules)) {
          await submodules.open(node);
          return;
        }
        const plan = richComparison(node.record);
        const empty = node.change.uri.with({ scheme: 'gitpane-empty', query: '' });
        const resolve = async side => {
          if (side.empty) return empty;
          const target = scopedFile(node.repo.rootUri.fsPath, side.filePath || side.gitPath);
          if (side.filePath) {
            const stat = fs.lstatSync(target);
            if (stat.isSymbolicLink()) {
              const current = () => {
                const repo = tree.git.repositories.find(item => item.rootUri.toString() === node.repo.rootUri.toString());
                if (!repo || !tree.isCurrentRepository(repo, node.repoToken)) return false;
                if (!node.parentWorktree) return true;
                const parent = tree.git.repositories.find(item => item.rootUri.toString() === node.parentWorktree.repoKey);
                return !!parent && tree.isCurrentRepository(parent, node.parentWorktree.repoToken) &&
                  tree.workspaceWorktrees(parent).some(item => item.path === node.parentWorktree.path) &&
                  tree.worktreeRepository(parent, node.parentWorktree.path)?.rootUri.toString() === repo.rootUri.toString();
              };
              const verify = async () => {
                const result = await backend.verifyChanges(node.repo.rootUri.fsPath, node.record);
                return validRecord(result?.change) && JSON.stringify(result.change) === JSON.stringify(node.record);
              };
              const revision = statusRevision(() => tree.backendStatuses.get(node.repo.rootUri.toString()), side.filePath);
              return links.create(vscode.Uri.file(target), node.repo.rootUri.fsPath, side.filePath,
                current, verify, revision);
            }
            if (!stat.isFile()) throw new Error('The changed file is unavailable.');
          }
          const uri = vscode.Uri.file(target);
          return side.filePath ? uri : api.toGitUri(uri, side.ref);
        };
        const left = await resolve(plan.left);
        const right = await resolve(plan.right);
        if (!await provider.waitForCurrentChange(node)) throw new Error('The change is no longer in the workspace.');
        const title = `${path.basename(node.change.uri.fsPath)} (${node.group})`;
        await vscode.commands.executeCommand('vscode.diff', left, right, title);
      } catch (error) {
        output.appendLine(`Could not open change: ${error.message}`);
        vscode.window.showErrorMessage(`Gitpane could not open the change: ${error.message}`);
      }
    })
  );
  await tree.refresh();
}

module.exports = { activate, GitpaneViewProvider, RepositoryTree };
