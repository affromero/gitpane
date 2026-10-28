'use strict';

const vscode = require('vscode');
const path = require('node:path');
const { execFile } = require('node:child_process');
const { promisify } = require('node:util');
const { canonicalPath, comparison, groups, isSubmodule, parseDiscovery, statusLabel, visibleRepository } = require('./model');

const execute = promisify(execFile);

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
    this.errors = [];
    this.failedStatuses = new Set();
    this.discovered = new Set();
    this.worktrees = new Set();
    this.busy = false;
    this.pending = false;
    this.disposed = false;
    this.subscriptions = [
      git.onDidOpenRepository(repo => { this.observe(repo); this.changed(); }),
      git.onDidCloseRepository(repo => { this.unobserve(repo); this.changed(); })
    ];
    for (const repo of git.repositories) this.observe(repo);
  }

  observe(repo) {
    const key = repo.rootUri.toString();
    if (this.listeners.has(key)) return;
    this.listeners.set(key, repo.state.onDidChange(() => this.changed()));
  }

  unobserve(repo) {
    const key = repo.rootUri.toString();
    this.listeners.get(key)?.dispose();
    this.listeners.delete(key);
  }

  changed() {
    if (!this.disposed) this.emitter.fire();
  }

  fail(message) {
    this.errors.push(message);
    this.output.appendLine(message);
  }

  isVisible(repo) {
    const workspacePaths = (vscode.workspace.workspaceFolders || [])
      .filter(folder => folder.uri.scheme === 'file').map(folder => folder.uri.fsPath);
    return visibleRepository(repo.rootUri.fsPath, this.discovered, this.worktrees, workspacePaths);
  }

  async refresh() {
    if (this.disposed) return;
    if (this.busy) {
      this.pending = true;
      return;
    }
    this.busy = true;
    this.errors = [];
    this.changed();
    try {
      const executable = vscode.workspace.getConfiguration('gitpane').get('executable', 'gitpane');
      let discoverySucceeded = false;
      try {
        const { stdout } = await execute(executable, ['discover'], { timeout: 30000, maxBuffer: 4 * 1024 * 1024 });
        this.discovered = new Set(parseDiscovery(stdout, path.isAbsolute).map(canonicalPath));
        discoverySucceeded = true;
      } catch (error) {
        this.fail(`Discovery failed; previous repository list may be stale: ${error.stderr?.trim() || error.message}`);
      }

      if (discoverySucceeded) this.worktrees = new Set();
      const opened = new Map(this.git.repositories.filter(repo => this.isVisible(repo)).map(repo => [canonicalPath(repo.rootUri.fsPath), repo]));
      await parallel([...this.discovered], 4, async repoPath => {
        if (opened.has(repoPath)) return;
        try {
          const repo = await this.git.openRepository(vscode.Uri.file(repoPath));
          if (!repo || canonicalPath(repo.rootUri.fsPath) !== repoPath) {
            this.fail(`Could not open repository: ${repoPath}`);
            return;
          }
          opened.set(repoPath, repo);
          this.observe(repo);
        } catch (error) {
          this.fail(`Could not open ${repoPath}: ${error.message}`);
        }
      });

      // Ask VS Code to refresh local status before reading its worktree list.
      await parallel([...opened.values()], 4, async repo => {
        try {
          await repo.status();
          this.failedStatuses.delete(repo.rootUri.toString());
        }
        catch (error) {
          this.failedStatuses.add(repo.rootUri.toString());
          this.fail(`Could not refresh ${repo.rootUri.fsPath}: ${error.message}`);
        }
      });
      const worktrees = discoverySucceeded ? new Set() : new Set(this.worktrees);
      for (const repo of opened.values()) {
        for (const worktree of repo.state.worktrees || []) {
          if (path.isAbsolute(worktree.path)) worktrees.add(canonicalPath(worktree.path));
        }
      }
      this.worktrees = worktrees;
      await parallel([...worktrees].filter(worktreePath => !opened.has(worktreePath)), 4, async worktreePath => {
        try {
          const repo = await this.git.openRepository(vscode.Uri.file(worktreePath));
          if (!repo || canonicalPath(repo.rootUri.fsPath) !== worktreePath) {
            this.fail(`Could not open worktree: ${worktreePath}`);
            return;
          }
          opened.set(worktreePath, repo);
          this.observe(repo);
          try {
            await repo.status();
            this.failedStatuses.delete(repo.rootUri.toString());
          }
          catch (error) {
            this.failedStatuses.add(repo.rootUri.toString());
            this.fail(`Could not refresh ${worktreePath}: ${error.message}`);
          }
        } catch (error) {
          this.fail(`Could not open worktree ${worktreePath}: ${error.message}`);
        }
      });
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
      const failures = this.errors.map(message => ({ type: 'error', message }));
      const repos = this.git.repositories.filter(repo => this.isVisible(repo)).sort((a, b) => {
        const aDirty = groups(a.state).length > 0;
        const bDirty = groups(b.state).length > 0;
        return Number(bDirty) - Number(aDirty) || a.rootUri.fsPath.localeCompare(b.rootUri.fsPath);
      });
      if (this.busy && !repos.length && !failures.length) return [{ type: 'info', message: 'Loading repositories…' }];
      if (!repos.length && !failures.length) return [{ type: 'info', message: 'No repositories found' }];
      return [...failures, ...repos.map(repo => ({ type: 'repo', repo }))];
    }
    if (node.type === 'repo') return groups(node.repo.state).map(group => ({ type: 'group', repo: node.repo, group }));
    if (node.type === 'group') return node.group.changes.map(change => ({ type: 'change', repo: node.repo, group: node.group.id, change }));
    return [];
  }

  getTreeItem(node) {
    if (node.type === 'error' || node.type === 'info') {
      const item = new vscode.TreeItem(node.message);
      item.iconPath = new vscode.ThemeIcon(node.type === 'error' ? 'warning' : 'info');
      item.tooltip = node.message;
      return item;
    }
    const root = node.repo.rootUri.fsPath;
    if (node.type === 'repo') {
      const changes = groups(node.repo.state).reduce((total, group) => total + group.changes.length, 0);
      const item = new vscode.TreeItem(path.basename(root), changes ? vscode.TreeItemCollapsibleState.Collapsed : vscode.TreeItemCollapsibleState.None);
      item.id = `repo:${node.repo.rootUri.toString()}`;
      const head = node.repo.state.HEAD;
      const branch = head?.name || (head?.commit ? head.commit.slice(0, 8) : 'unborn');
      const status = this.failedStatuses.has(node.repo.rootUri.toString()) ? 'status unavailable'
        : this.busy ? 'refreshing…' : changes ? `${changes} changes` : 'clean';
      item.description = `${branch} · ${status}`;
      item.tooltip = root;
      item.iconPath = new vscode.ThemeIcon('repo');
      return item;
    }
    if (node.type === 'group') {
      const item = new vscode.TreeItem(node.group.label, vscode.TreeItemCollapsibleState.Expanded);
      item.id = `group:${node.repo.rootUri.toString()}:${node.group.id}`;
      item.description = String(node.group.changes.length);
      return item;
    }
    const item = new vscode.TreeItem(path.basename(node.change.uri.fsPath));
    item.id = `file:${node.repo.rootUri.toString()}:${node.group}:${node.change.uri.toString()}`;
    item.resourceUri = node.change.uri;
    const relative = path.relative(root, node.change.uri.fsPath);
    item.description = `${statusLabel(node.change.status)}  ${path.dirname(relative) === '.' ? '' : path.dirname(relative)}`.trim();
    item.tooltip = relative;
    item.command = { command: 'gitpane.openChange', title: 'Open Change', arguments: [node] };
    return item;
  }

  dispose() {
    this.disposed = true;
    for (const subscription of this.subscriptions) subscription.dispose();
    for (const subscription of this.listeners.values()) subscription.dispose();
    this.emitter.dispose();
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
  const view = vscode.window.createTreeView('gitpane.repositories', { treeDataProvider: tree });
  context.subscriptions.push(tree, view,
    vscode.workspace.registerTextDocumentContentProvider('gitpane-empty', { provideTextDocumentContent: () => '' }),
    vscode.commands.registerCommand('gitpane.refresh', () => tree.refresh()),
    vscode.commands.registerCommand('gitpane.openChange', async node => {
      try {
        if (isSubmodule(node.repo.rootUri.fsPath, node.change.uri.fsPath, node.repo.state.submodules)) {
          throw new Error('Submodule changes cannot be opened as a file diff yet.');
        }
        const plan = comparison(node.group, node.change);
        if (plan.open) {
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
    }),
    vscode.workspace.onDidChangeConfiguration(event => {
      if (event.affectsConfiguration('gitpane.executable')) tree.refresh();
    }),
    vscode.workspace.onDidChangeWorkspaceFolders(() => tree.refresh())
  );
  await tree.refresh();
}

module.exports = { activate, RepositoryTree };
