'use strict';

const vscode = require('vscode');
const path = require('node:path');
const { randomUUID } = require('node:crypto');
const { canonicalPath, changedFileCount, comparison, groups, isSubmodule, statusLabel, visibleRepository } = require('./model');

async function parallel(items, limit, action) {
  let next = 0;
  await Promise.all(Array.from({ length: Math.min(limit, items.length) }, async () => {
    while (next < items.length) {
      const item = items[next++];
      await action(item);
    }
  }));
}

class RepositoryTree {
  constructor(git, output) {
    this.git = git;
    this.output = output;
    this.emitter = new vscode.EventEmitter();
    this.onDidChangeTreeData = this.emitter.event;
    this.listeners = new Map();
    this.identities = new Map();
    this.groupCache = new Map();
    this.workspacePaths = [];
    this.changeTimer = undefined;
    this.pollError = undefined;
    this.failedStatuses = new Set();
    this.statusErrors = new Map();
    this.busy = false;
    this.pending = false;
    this.disposed = false;
    this.visible = false;
    this.pollTimer = undefined;
    this.pollPromise = undefined;
    this.refreshTimer = undefined;
    this.subscriptions = [
      git.onDidOpenRepository(repo => {
        this.identities.delete(repo.rootUri.fsPath);
        this.observe(repo);
        if (this.isVisible(repo)) this.scheduleRefresh();
      }),
      git.onDidCloseRepository(repo => {
        const wasVisible = this.isVisible(repo);
        this.unobserve(repo);
        this.identities.delete(repo.rootUri.fsPath);
        if (wasVisible) this.scheduleChanged();
      }),
      vscode.workspace.onDidChangeWorkspaceFolders(() => {
        this.identities.clear();
        this.updateWorkspacePaths();
        this.changed();
        this.refresh().catch(error => {
          this.fail(`Could not refresh repository status: ${error.message}`);
          this.changed();
        });
      })
    ];
    this.updateWorkspacePaths();
    for (const repo of git.repositories) this.observe(repo);
  }

  observe(repo) {
    const key = repo.rootUri.toString();
    if (this.listeners.has(key)) return;
    this.listeners.set(key, repo.state.onDidChange(() => {
      this.groupCache.delete(key);
      if (this.isVisible(repo)) this.scheduleChanged();
    }));
  }

  unobserve(repo) {
    const key = repo.rootUri.toString();
    this.listeners.get(key)?.dispose();
    this.listeners.delete(key);
    this.groupCache.delete(key);
    this.failedStatuses.delete(key);
    this.statusErrors.delete(key);
  }

  changed() {
    if (!this.disposed) this.emitter.fire();
  }

  scheduleChanged() {
    if (this.disposed || this.changeTimer !== undefined) return;
    this.changeTimer = setTimeout(() => {
      this.changeTimer = undefined;
      this.changed();
    }, 50);
  }

  scheduleRefresh() {
    if (this.disposed || this.refreshTimer !== undefined) return;
    this.refreshTimer = setTimeout(() => {
      this.refreshTimer = undefined;
      this.refresh().catch(error => {
        this.fail(`Could not refresh repository status: ${error.message}`);
        this.changed();
      });
    }, 100);
  }

  identify(value) {
    if (!this.identities.has(value)) this.identities.set(value, canonicalPath(value));
    return this.identities.get(value);
  }

  updateWorkspacePaths() {
    this.workspacePaths = (vscode.workspace.workspaceFolders || [])
      .filter(folder => folder.uri.scheme === 'file').map(folder => this.identify(folder.uri.fsPath));
  }

  groupsFor(repo) {
    const key = repo.rootUri.toString();
    if (!this.groupCache.has(key)) this.groupCache.set(key, groups(repo.state));
    return this.groupCache.get(key);
  }

  fail(message) {
    if (this.pollError !== message) this.output.appendLine(message);
    this.pollError = message;
  }

  statusFailed(repo, error) {
    if (this.disposed) return;
    const key = repo.rootUri.toString();
    const message = `Could not refresh ${repo.rootUri.fsPath}: ${error.message}`;
    if (this.statusErrors.get(key) !== message) this.output.appendLine(message);
    this.statusErrors.set(key, message);
    this.failedStatuses.add(key);
  }

  statusSucceeded(repo) {
    if (this.disposed) return;
    const key = repo.rootUri.toString();
    this.statusErrors.delete(key);
    this.failedStatuses.delete(key);
    this.groupCache.delete(key);
  }

  setVisible(visible) {
    this.visible = visible;
    if (!visible && this.pollTimer !== undefined) {
      clearTimeout(this.pollTimer);
      this.pollTimer = undefined;
    }
    if (visible) this.schedulePoll();
  }

  schedulePoll() {
    if (this.disposed || !this.visible || this.pollTimer !== undefined || this.pollPromise) return;
    this.pollTimer = setTimeout(() => {
      this.pollTimer = undefined;
      this.pollPromise = this.reconcile().catch(error => {
        this.fail(`Could not refresh repository status: ${error.message}`);
        this.changed();
      });
      this.pollPromise.finally(() => {
        this.pollPromise = undefined;
        this.schedulePoll();
      });
    }, 5000);
  }

  async reconcile() {
    if (this.busy || this.disposed || !this.visible) return;
    const repos = this.git.repositories.filter(repo => this.isVisible(repo));
    await parallel(repos, 4, async repo => {
      if (this.disposed || !this.visible || !this.isVisible(repo)) return;
      try {
        await repo.status();
        this.statusSucceeded(repo);
      } catch (error) {
        this.statusFailed(repo, error);
      }
    });
    this.pollError = undefined;
    this.scheduleChanged();
  }

  isVisible(repo) {
    return repo.rootUri.scheme === 'file' && visibleRepository(repo.rootUri.fsPath,
      this.workspacePaths, value => this.identify(value));
  }

  async refresh() {
    if (this.pollPromise) await this.pollPromise;
    if (this.disposed) return;
    if (this.busy) {
      this.pending = true;
      return;
    }
    this.busy = true;
    this.pollError = undefined;
    this.identities.clear();
    this.updateWorkspacePaths();
    this.changed();
    try {
      const repos = this.git.repositories.filter(repo => this.isVisible(repo));
      await parallel(repos, 4, async repo => {
        if (this.disposed || !this.isVisible(repo)) return;
        try {
          await repo.status();
          this.statusSucceeded(repo);
        }
        catch (error) {
          this.statusFailed(repo, error);
        }
      });
      const active = new Set(repos.map(repo => repo.rootUri.toString()));
      for (const key of this.statusErrors.keys()) {
        if (!active.has(key)) {
          this.statusErrors.delete(key);
          this.failedStatuses.delete(key);
        }
      }
    } finally {
      this.busy = false;
      this.changed();
      if (this.pending && !this.disposed) {
        this.pending = false;
        await this.refresh();
      }
    }
  }

  getChildren(node) {
    if (!node) {
      const failures = [this.pollError, ...this.statusErrors.values()].filter(Boolean).map(message => ({ type: 'error', message }));
      const repos = this.git.repositories.filter(repo => this.isVisible(repo)).map(repo => ({
        repo, dirty: this.groupsFor(repo).length > 0
      })).sort((a, b) => Number(b.dirty) - Number(a.dirty) || a.repo.rootUri.fsPath.localeCompare(b.repo.rootUri.fsPath));
      if (this.busy && !repos.length && !failures.length) return [{ type: 'info', message: 'Loading repositories…' }];
      if (!repos.length && !failures.length) return [{ type: 'info', message: this.workspacePaths.length
        ? 'No repositories found in the workspace' : 'Open a folder or workspace to see repositories' }];
      const names = new Map();
      for (const { repo } of repos) {
        const name = path.basename(repo.rootUri.fsPath);
        names.set(name, (names.get(name) || 0) + 1);
      }
      return [...failures, ...repos.map(({ repo }) => ({ type: 'repo', repo,
        duplicate: names.get(path.basename(repo.rootUri.fsPath)) > 1 }))];
    }
    if (node.type === 'repo') return this.groupsFor(node.repo).map(group => ({ type: 'group', repo: node.repo, group }));
    if (node.type === 'group') return node.group.changes.map(change => ({ type: 'change', repo: node.repo, group: node.group.id, change }));
    return [];
  }

  dispose() {
    this.disposed = true;
    if (this.changeTimer !== undefined) clearTimeout(this.changeTimer);
    if (this.refreshTimer !== undefined) clearTimeout(this.refreshTimer);
    if (this.pollTimer !== undefined) clearTimeout(this.pollTimer);
    for (const subscription of this.subscriptions) subscription.dispose();
    for (const subscription of this.listeners.values()) subscription.dispose();
    this.emitter.dispose();
  }
}

class GitpaneViewProvider {
  constructor(tree, extensionUri) {
    this.tree = tree;
    this.extensionUri = extensionUri;
    this.view = undefined;
    this.ready = false;
    this.sessionId = randomUUID();
    this.nextId = 0;
    this.ids = new Map();
    this.changes = new Map();
    this.viewDisposables = [];
    this.subscription = tree.onDidChangeTreeData(() => this.publish());
  }

  idFor(key, live) {
    live.add(key);
    if (!this.ids.has(key)) this.ids.set(key, `item-${this.sessionId}-${++this.nextId}`);
    return this.ids.get(key);
  }

  snapshot() {
    const live = new Set();
    const changes = new Map();
    const children = this.tree.getChildren();
    const repositories = children.filter(node => node.type === 'repo').map(node => {
      const repo = node.repo;
      const root = repo.rootUri.fsPath;
      const repoKey = repo.rootUri.toString();
      const id = this.idFor(JSON.stringify(['repo', repoKey]), live);
      const head = repo.state.HEAD;
      const changeGroups = this.tree.getChildren(node);
      const groupData = changeGroups.map(groupNode => ({
        id: groupNode.group.id,
        label: groupNode.group.label,
        files: this.tree.getChildren(groupNode).map(fileNode => {
          const change = fileNode.change;
          const fileId = this.idFor(JSON.stringify(['file', repoKey, groupNode.group.id, change.uri.toString()]), live);
          changes.set(fileId, fileNode);
          return { id: fileId, name: path.basename(change.uri.fsPath),
            path: path.relative(root, change.uri.fsPath), status: statusLabel(change.status) };
        })
      }));
      return {
        id,
        name: node.duplicate ? `${path.basename(root)} (${path.dirname(root)})` : path.basename(root),
        branch: head?.name || (head?.commit ? head.commit.slice(0, 8) : 'unborn'),
        dirty: changedFileCount(this.tree.groupsFor(repo)) > 0,
        ahead: head?.ahead || 0,
        behind: head?.behind || 0,
        worktree: repo.kind === 'worktree',
        statusUnavailable: this.tree.failedStatuses.has(repoKey),
        groups: groupData
      };
    });
    for (const key of this.ids.keys()) if (!live.has(key)) this.ids.delete(key);
    this.changes = changes;
    return {
      type: 'snapshot', repositories,
      errors: children.filter(node => node.type === 'error').map(node => node.message),
      emptyMessage: children.find(node => node.type === 'info')?.message,
      loading: this.tree.busy
    };
  }

  publish() {
    if (!this.view || !this.ready) return;
    this.view.webview.postMessage(this.snapshot());
  }

  currentChange(node) {
    if (!node || !this.tree.isVisible(node.repo)) return undefined;
    const repoKey = node.repo.rootUri.toString();
    const repo = this.tree.git.repositories.find(item => item.rootUri.toString() === repoKey);
    if (!repo) return undefined;
    const group = this.tree.groupsFor(repo).find(item => item.id === node.group);
    const change = group?.changes.find(item => item.uri.toString() === node.change.uri.toString());
    return change ? { type: 'change', repo, group: node.group, change } : undefined;
  }

  resolveWebviewView(view) {
    for (const disposable of this.viewDisposables) disposable.dispose();
    this.viewDisposables = [];
    this.view = view;
    this.ready = false;
    view.webview.options = {
      enableScripts: true,
      localResourceRoots: [vscode.Uri.joinPath(this.extensionUri, 'src', 'webview')]
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
          try { await this.tree.refresh(); }
          catch (error) {
            this.tree.fail(`Could not refresh repository status: ${error.message}`);
            this.tree.changed();
          }
        } else if (message?.type === 'openChange' && typeof message.id === 'string') {
          const node = this.currentChange(this.changes.get(message.id));
          if (node) await vscode.commands.executeCommand('gitpane.openChange', node);
        }
      }),
      view.onDidChangeVisibility(() => this.tree.setVisible(view.visible)),
      view.onDidDispose(() => {
        if (this.view !== view) return;
        this.tree.setVisible(false);
        this.view = undefined;
        this.ready = false;
        this.changes.clear();
      })
    );
  }

  dispose() {
    this.tree.setVisible(false);
    this.subscription.dispose();
    for (const disposable of this.viewDisposables) disposable.dispose();
    this.viewDisposables = [];
    this.view = undefined;
    this.changes.clear();
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
  const tree = new RepositoryTree(api, output);
  const provider = new GitpaneViewProvider(tree, context.extensionUri);
  context.subscriptions.push(tree, provider,
    vscode.window.registerWebviewViewProvider('gitpane.repositories', provider),
    vscode.workspace.registerTextDocumentContentProvider('gitpane-empty', { provideTextDocumentContent: () => '' }),
    vscode.commands.registerCommand('gitpane.refresh', () => tree.refresh()),
    vscode.commands.registerCommand('gitpane.openChange', async node => {
      try {
        if (isSubmodule(node.repo.rootUri.fsPath, node.change.uri.fsPath, node.repo.state.submodules)) {
          throw new Error('Submodule changes cannot be opened as a file diff yet.');
        }
        const plan = comparison(node.group, node.change);
        if (plan.open) {
          try { await vscode.workspace.fs.stat(plan.open); }
          catch (error) { throw new Error(`Conflict file is unavailable: ${error.message}`); }
          await vscode.commands.executeCommand('vscode.open', plan.open);
          return;
        }
        const empty = node.change.uri.with({ scheme: 'gitpane-empty', query: '' });
        const resolve = side => side.empty ? empty : side.file || api.toGitUri(side.git, side.ref);
        const title = `${path.basename(node.change.uri.fsPath)} (${node.group})`;
        await vscode.commands.executeCommand('vscode.diff', resolve(plan.left), resolve(plan.right), title);
      } catch (error) {
        output.appendLine(`Could not open change: ${error.message}`);
        vscode.window.showErrorMessage(`Gitpane could not open the change: ${error.message}`);
      }
    })
  );
  await tree.refresh();
}

module.exports = { activate, GitpaneViewProvider, RepositoryTree };
