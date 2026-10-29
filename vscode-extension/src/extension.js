'use strict';

const vscode = require('vscode');
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
const { changedFileCount, comparison, isSubmodule, statusLabel } = require('./model');
const { RepositoryTree } = require('./repository-tree');

class GitpaneViewProvider {
  constructor(tree, extensionUri) {
    this.tree = tree;
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
    const worktrees = new Map();
    const repositoriesById = new Map();
    const groupsFor = (repo, node, keyPrefix, parentWorktree) => {
      const root = repo.rootUri.fsPath;
      const repoKey = repo.rootUri.toString();
      const repoToken = this.tree.repositoryToken(repo);
      return this.tree.getChildren(node).map(groupNode => ({
        id: groupNode.group.id,
        label: groupNode.group.label,
        files: this.tree.getChildren(groupNode).map(fileNode => {
          const change = fileNode.change;
          const fileId = this.idFor(JSON.stringify([keyPrefix, repoKey, repoToken, groupNode.group.id, change.uri.toString()]), live);
          changes.set(fileId, { ...fileNode, repoToken, parentWorktree });
          return { id: fileId, name: path.basename(change.uri.fsPath),
            path: path.relative(root, change.uri.fsPath), status: statusLabel(change.status) };
        })
      }));
    };
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
        const childToken = child && this.tree.repositoryToken(child);
        const worktreeId = this.idFor(JSON.stringify(['worktree', repoKey, repoToken, item.path, childToken]), live);
        const childRepoId = childRepoKey && this.idFor(JSON.stringify(['repo', childRepoKey, childToken]), live);
        if (childRepoId) repositoriesById.set(childRepoId, { repoKey: childRepoKey, repoToken: childToken });
        worktrees.set(worktreeId, { repoKey, repoToken, path: item.path, childRepoId, childToken });
        return { id: worktreeId, name: path.basename(item.path), branch: item.branch,
          dirty: item.is_dirty, fileCount: item.file_count, ahead: item.ahead, behind: item.behind,
          available: !!child, groups: child ? groupsFor(child, { type: 'repo', repo: child },
            `worktree-file:${worktreeId}`, { repoKey, repoToken, path: item.path }) : [] };
      });
      const groupData = groupsFor(repo, node, 'file');
      return {
        id,
        name: node.duplicate ? `${path.basename(root)} (${path.dirname(root)})` : path.basename(root),
        branch: (status?.branch === '(no branch)' && !status.head_oid && head?.name) ||
          status?.branch || head?.name || (head?.commit ? head.commit.slice(0, 8) : 'unborn'),
        dirty: !!status?.is_dirty || changedFileCount(this.tree.groupsFor(repo)) > 0,
        ahead: status?.ahead || 0,
        behind: status?.behind || 0,
        worktree: repo.kind === 'worktree',
        statusUnavailable: this.tree.failedStatuses.has(repoKey) || !status,
        stashCount: status?.stashes.length || 0,
        stashes,
        worktreeCount: worktreeData.length,
        worktrees: worktreeData,
        hasDirtySubmodules: status?.has_dirty_submodules || false,
        hasUnpushedSubmodules: status?.has_unpushed_submodules || false,
        groups: groupData
      };
    });
    for (const key of this.ids.keys()) if (!live.has(key)) this.ids.delete(key);
    this.changes = changes;
    this.worktrees = worktrees;
    this.repositories = repositoriesById;
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
    const group = this.tree.groupsFor(repo).find(item => item.id === node.group);
    const change = group?.changes.find(item => item.uri.toString() === node.change.uri.toString());
    return change ? { type: 'change', repo, repoToken: node.repoToken, group: node.group, change, parentWorktree: node.parentWorktree } : undefined;
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
    return this.actionRepository(this.currentRepository(id));
  }

  resolveChange(id) {
    const node = this.currentChange(this.changes.get(id));
    const repo = node && this.actionRepository(node.repo);
    if (!repo) return undefined;
    const relative = path.relative(repo.repoPath, node.change.uri.fsPath);
    if (!relative || path.isAbsolute(relative) || relative === '..' || relative.startsWith(`..${path.sep}`)) return undefined;
    return { ...repo, path: relative, group: node.group, status: node.change.status, uri: node.change.uri, node,
      isSubmodule: isSubmodule(repo.repoPath, node.change.uri.fsPath, node.repo.state.submodules) };
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
          try { await this.tree.refresh(true); }
          catch (error) {
            this.tree.fail(`Could not refresh repository status: ${error.message}`);
            this.tree.changed();
          }
        } else if (message?.type === 'openChange' && typeof message.id === 'string') {
          const node = this.currentChange(this.changes.get(message.id));
          if (node) await vscode.commands.executeCommand('gitpane.openChange', node);
        } else if (message?.type === 'openGraph' && typeof message.id === 'string') {
          if (this.currentRepository(message.id)) await vscode.commands.executeCommand('gitpane.openGraph', message.id);
        } else if (message?.type === 'openWorktreeGraph' && typeof message.id === 'string') {
          await this.openWorktreeGraph?.(message.id);
        } else if (message?.type === 'menu' && typeof message.id === 'string') {
          await this.actions?.showMenu(message.kind, message.id);
        }
      }),
      view.onDidChangeVisibility(() => this.tree.setVisible(view.visible)),
      view.onDidDispose(() => {
        if (this.view !== view) return;
        this.tree.setVisible(false);
        this.view = undefined;
        this.ready = false;
        this.repositories.clear();
        this.changes.clear();
        this.worktrees.clear();
      })
    );
  }

  dispose() {
    this.tree.setVisible(false);
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
  const tree = new RepositoryTree(api, output, backend, context.workspaceState?.get('gitpane.sortOrder'));
  const provider = new GitpaneViewProvider(tree, context.extensionUri);
  const graph = new GraphProvider(tree, backend, api, context.extensionUri);
  const review = new BranchReviewProvider(tree, backend, api);
  const github = new GithubProvider(tree, backend);
  const sessions = new SessionProvider(tree, backend);
  const terminals = new RepoTerminalProvider(tree);
  const launchers = new LauncherProvider(tree, backend);
  const submodules = new SubmoduleProvider(tree, backend, api, {
    resolveChange: node => provider.currentChange(node),
    openGraph: (repo, guard) => graph.open(repo, guard)
  });
  const mutations = new MutationManager({
    getSnapshot: () => backend.mutationSnapshot(),
    isCurrent: (snapshot, repo) => backend.isCurrentMutation(snapshot, repo),
    onStderr: text => output.append(text)
  });
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
    previewFileAction: (repo, target, operation) => backend.fileActionPreview(repo, target, operation),
    planWorktree: (repo, branch) => backend.planWorktree(repo, branch),
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
    refresh: () => tree.refresh() });
  context.subscriptions.push(backend, tree, provider, graph, review, github, sessions, terminals, launchers, submodules, mutations,
    vscode.window.registerWebviewViewProvider('gitpane.repositories', provider),
    vscode.workspace.registerTextDocumentContentProvider('gitpane-empty', { provideTextDocumentContent: () => '' }),
    vscode.commands.registerCommand('gitpane.refresh', () => tree.refresh(true)),
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
        let node = provider.currentChange(selected);
        if (!node) throw new Error('The change is no longer in the workspace.');
        if (isSubmodule(node.repo.rootUri.fsPath, node.change.uri.fsPath, node.repo.state.submodules)) {
          await submodules.open(node);
          return;
        }
        const plan = comparison(node.group, node.change);
        if (plan.open) {
          try { await vscode.workspace.fs.stat(plan.open); }
          catch (error) { throw new Error(`Conflict file is unavailable: ${error.message}`); }
          node = provider.currentChange(node);
          if (!node) throw new Error('The change is no longer in the workspace.');
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
