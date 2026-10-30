'use strict';

const { receiveViewMessage } = require('./view-messages');

class WorkspaceViews {
  constructor(vscode, context, provider, graph, openGraph) {
    this.vscode = vscode;
    this.context = context;
    this.provider = provider;
    this.graph = graph;
    this.openGraph = openGraph;
    this.owners = new Map();
    this.pages = { repositories: new Set(), changes: new Set() };
    this.selectionEpoch = 0;
    this.selectedId = undefined;
    const remembered = context.workspaceState?.get('gitpane.selectedRepository');
    this.preferredPath = typeof remembered === 'string' ? remembered : undefined;
    this.fallbackSelection = false;
    this.changes = undefined;
    this.graphView = undefined;
    this.graphOwner = undefined;
    this.graphDisposables = [];
    this.publishing = false;
    this.disposed = false;
    provider.workspaceViews = this;
  }

  get visible() {
    return this.provider.view?.visible === true || this.changes?.view.visible === true || this.graphView?.visible === true;
  }

  updateOwners(snapshot) {
    const owners = new Map();
    for (const repo of snapshot.repositories) {
      owners.set(repo.id, { owner: repo, kind: 'repo' });
      for (const worktree of repo.worktrees || []) {
        owners.set(worktree.id, { owner: worktree, kind: 'worktree', parent: repo.id });
      }
    }
    this.owners = owners;
  }

  current(id) {
    const entry = this.owners.get(id);
    if (!entry) return undefined;
    if (this.provider.currentOwner(id)) return entry;
    if (entry.kind === 'worktree' && entry.owner.available === false && this.provider.currentRepository(entry.parent)) return entry;
    return undefined;
  }

  setSelection(id, remember = false) {
    if (this.selectedId !== id) {
      if (this.selectedId) this.releaseChanges('changes', this.selectedId);
      this.selectedId = id;
      this.selectionEpoch++;
    }
    const repo = id && this.provider.currentOwner(id);
    const selectedPath = repo?.rootUri.fsPath || (id && this.current(id)?.kind === 'worktree' ? this.provider.worktrees.get(id)?.path : undefined);
    if (remember && selectedPath) {
      this.fallbackSelection = false;
      this.preferredPath = selectedPath;
      const saved = this.context.workspaceState?.update('gitpane.selectedRepository', this.preferredPath);
      saved?.catch(error => this.vscode.window.showErrorMessage(`Gitpane could not remember the selected repository: ${error.message}`));
    }
  }

  select(id) {
    if (!this.current(id)) return this.publish();
    this.setSelection(id, true);
    this.publish();
  }

  accepts(message, surface) {
    if (!message || typeof message.type !== 'string') return false;
    if (surface === 'repositories') {
      if (this.changes?.view.visible === true && ['loadChanges', 'changesPage', 'searchChanges'].includes(message.type)) return false;
      return message.type === 'ready' || this.provider.view?.visible !== false;
    }
    if (!this.changes?.ready || this.changes.view.visible === false ||
        message.ownerId !== this.selectedId || message.selectionEpoch !== this.selectionEpoch) return false;
    if (message.type === 'refresh') return true;
    if (!this.current(this.selectedId)) return false;
    if (['loadChanges', 'releaseChanges', 'changesPage', 'searchChanges', 'shortcut'].includes(message.type) &&
        message.id !== this.selectedId) return false;
    if (message.type === 'openChange' || (message.type === 'menu' && message.kind === 'file')) {
      return this.provider.changes.get(message.id)?.ownerId === this.selectedId;
    }
    return ['refresh', 'loadChanges', 'releaseChanges', 'changesPage', 'searchChanges', 'shortcut'].includes(message.type);
  }

  messageGuard(message, surface) {
    if (surface === 'repositories') return () => !this.disposed;
    const view = this.changes;
    return () => !this.disposed && this.changes === view && this.accepts(message, surface);
  }

  retainChanges(surface, id) { this.pages[surface].add(id); }

  releaseChanges(surface, id) {
    this.pages[surface].delete(id);
    if (!Object.values(this.pages).some(ids => ids.has(id))) {
      this.provider.searchRequests.delete(id);
      this.provider.pager.release(id);
    }
  }

  visibilityChanged() {
    if (this.disposed) return;
    if (this.provider.view?.visible !== true || this.changes?.view.visible === true) {
      for (const id of [...this.pages.repositories]) this.releaseChanges('repositories', id);
    }
    if (this.changes?.view.visible !== true) {
      for (const id of [...this.pages.changes]) this.releaseChanges('changes', id);
    }
    this.provider.tree.setVisible(this.visible);
    if (!this.visible && this.provider.refreshChangesTimer !== undefined) {
      clearTimeout(this.provider.refreshChangesTimer);
      this.provider.refreshChangesTimer = undefined;
    }
    this.publish();
    this.provider.scheduleChangesRefresh();
  }

  publish() {
    if (this.disposed || this.provider.disposed || this.publishing) return;
    this.publishing = true;
    try {
      const snapshot = this.provider.snapshot();
      this.updateOwners(snapshot);
      const preferred = [...this.owners.keys()].find(id => this.current(id) &&
        this.provider.currentOwner(id)?.rootUri.fsPath === this.preferredPath);
      if (!this.current(this.selectedId) || (this.fallbackSelection && preferred)) {
        this.setSelection(preferred || snapshot.repositories.find(repo => this.current(repo.id))?.id);
        this.fallbackSelection = !!this.preferredPath && !preferred;
      }
      const selected = this.current(this.selectedId);
      const common = { ...snapshot, selectedOwnerId: this.selectedId, selectionEpoch: this.selectionEpoch };
      if (this.provider.ready && this.provider.view?.visible !== false) {
        this.provider.view?.webview.postMessage({ ...common, separateChanges: this.changes?.view.visible === true });
      }
      if (this.changes?.ready && this.changes.view.visible !== false) {
        this.changes.view.title = selected ? `Changes: ${selected.owner.name || selected.owner.branch}` : 'Changes';
        this.changes.view.webview.postMessage({ ...common, selectedOwner: selected?.owner, selectedKind: selected?.kind });
      }
      const blocked = !selected || selected.owner.available === false || selected.owner.statusUnavailable;
      const graphOwner = `${this.selectionEpoch}:${blocked}`;
      if (this.graphOwner !== graphOwner) {
        this.graphOwner = graphOwner;
        const epoch = this.selectionEpoch;
        const ownerId = this.selectedId;
        if (blocked) this.graph.clearSelection(selected ? 'Repository status is unavailable.' : 'Select a repository to see its commit graph.', !!selected);
        else {
          Promise.resolve().then(() => this.openGraph(ownerId, selected.kind, () =>
            !this.disposed && this.selectionEpoch === epoch && this.selectedId === ownerId && !!this.current(ownerId)))
            .catch(error => {
              if (this.selectionEpoch === epoch) this.graph.invalidate(error.message);
            });
        }
      }
    } finally { this.publishing = false; }
  }

  resolveChangesView(view) {
    const previous = this.changes;
    if (previous) for (const disposable of previous.disposables) disposable.dispose();
    const state = { view, ready: false, disposables: [] };
    this.changes = state;
    view.webview.options = { enableScripts: true,
      localResourceRoots: [this.vscode.Uri.joinPath(this.context.extensionUri, 'src')] };
    view.webview.html = require('./webview/html').getHtml(view.webview, this.context.extensionUri, 'changes');
    state.disposables.push(view.webview.onDidReceiveMessage(message => {
      if (this.changes !== state) return;
      if (message?.type === 'ready') {
        state.ready = true;
        this.publish();
      } else return receiveViewMessage(this.provider, this.vscode, message, 'changes');
    }), view.onDidChangeVisibility(() => this.visibilityChanged()), view.onDidDispose(() => {
      if (this.changes !== state) return;
      for (const disposable of state.disposables) disposable.dispose();
      this.changes = undefined;
      this.visibilityChanged();
    }));
    this.visibilityChanged();
  }

  resolveGraphView(view) {
    for (const disposable of this.graphDisposables) disposable.dispose();
    this.graphView = view;
    this.graph.resolveWebviewView(view);
    this.graphDisposables = [view.onDidChangeVisibility(() => this.visibilityChanged()), view.onDidDispose(() => {
      if (this.graphView !== view) return;
      this.graphView = undefined;
      this.visibilityChanged();
    })];
    this.visibilityChanged();
  }

  dispose() {
    this.disposed = true;
    for (const disposable of this.changes?.disposables || []) disposable.dispose();
    for (const disposable of this.graphDisposables) disposable.dispose();
    this.graph.dispose();
    this.changes = undefined;
    this.graphView = undefined;
    this.provider.workspaceViews = undefined;
    this.owners.clear();
  }
}

module.exports = { WorkspaceViews };
