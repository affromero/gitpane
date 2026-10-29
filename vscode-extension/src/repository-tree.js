'use strict';

const vscode = require('vscode');
const fs = require('node:fs');
const path = require('node:path');
const { groups } = require('./model');
const { relativeFile } = require('./change-pager');
const { SessionLiveness } = require('./session-liveness');

function directoryIdentity(value) {
  try {
    const real = fs.realpathSync.native(value);
    const stat = fs.statSync(real, { bigint: true });
    if (!stat.isDirectory()) return undefined;
    return { path: process.platform === 'win32' ? real.toLowerCase() : real, dev: stat.dev, ino: stat.ino };
  } catch { return undefined; }
}

function sameIdentity(a, b) {
  return !!a && !!b && a.path === b.path && a.dev === b.dev && a.ino === b.ino;
}

function identityKey(identity) {
  return identity && `${identity.path}\0${identity.dev}\0${identity.ino}`;
}

function inside(parent, child) {
  const relative = path.relative(parent, child);
  return relative !== '..' && !relative.startsWith(`..${path.sep}`) && !path.isAbsolute(relative);
}

function compareWorktreeAnchors(a, b) {
  return Number(a.kind === 'worktree') - Number(b.kind === 'worktree') ||
    a.rootUri.fsPath.localeCompare(b.rootUri.fsPath);
}

async function parallel(items, limit, action) {
  let next = 0;
  await Promise.all(Array.from({ length: Math.min(limit, items.length) }, async () => {
    while (next < items.length) {
      const item = items[next++];
      await action(item);
    }
  }));
}

function validBackendStatus(status) {
  const count = value => Number.isSafeInteger(value) && value >= 0;
  return status && typeof status.common_dir === 'string' && path.isAbsolute(status.common_dir) &&
    typeof status.branch === 'string' && typeof status.is_dirty === 'boolean' &&
    count(status.ahead) && count(status.behind) && Array.isArray(status.stashes) &&
    Array.isArray(status.worktrees) && status.worktrees.every(item => item && typeof item.path === 'string' && path.isAbsolute(item.path)) &&
    typeof status.has_dirty_submodules === 'boolean' && typeof status.has_unpushed_submodules === 'boolean' &&
    Array.isArray(status.files) && Array.isArray(status.watched_errors) &&
    count(status.file_count_lower_bound) && typeof status.files_truncated === 'boolean';
}

const SORT_ORDERS = new Set(['workspace', 'alphabetical', 'reverse', 'dirty']);

class RepositoryTree {
  constructor(git, output, backend, sortOrder = 'workspace') {
    this.git = git;
    this.output = output;
    this.backend = backend;
    this.sortOrder = SORT_ORDERS.has(sortOrder) ? sortOrder : 'workspace';
    this.sortGeneration = 0;
    this.sortTimer = undefined;
    this.sortRanks = new Map();
    this.sortError = undefined;
    this.emitter = new vscode.EventEmitter();
    this.onDidChangeTreeData = this.emitter.event;
    this.listeners = new Map();
    this.repositoryIdentities = new Map();
    this.repositoryTokens = new Map();
    this.repositoryRevisions = new Map();
    this.nextRepositoryToken = 0;
    this.sharedWorkspaceIdentities = backend.workspaceIdentities instanceof Map;
    this.workspaceIdentities = this.sharedWorkspaceIdentities ? backend.workspaceIdentities : new Map();
    this.worktreeOpenings = new Map();
    this.groupCache = new Map();
    this.backendStatuses = new Map();
    this.watchedPathsFor = () => [];
    this.watchTokenFor = () => undefined;
    this.backendErrors = new Map();
    this.backendGeneration = 0;
    this.backendError = undefined;
    this.workspacePaths = [];
    this.changeTimer = undefined;
    this.pollError = undefined;
    this.failedStatuses = new Set();
    this.statusErrors = new Map();
    this.busy = false;
    this.pending = false;
    this.refreshWaiters = [];
    this.allRefreshWaiters = new Set();
    this.disposed = false;
    this.visible = false;
    this.pollTimer = undefined;
    this.pollPromise = undefined;
    this.refreshTimer = undefined;
    this.subscriptions = [
      git.onDidOpenRepository(repo => {
        this.observe(repo);
        if (this.isVisible(repo)) this.scheduleRefresh();
      }),
      git.onDidCloseRepository(repo => {
        const wasVisible = this.isVisible(repo);
        this.unobserve(repo);
        if (wasVisible) this.scheduleChanged();
      }),
      backend.onDidChange(state => this.backendChanged(state)),
      vscode.workspace.onDidChangeWorkspaceFolders(() => {
        this.backendGeneration++;
        this.backendStatuses.clear();
        this.backendErrors.clear();
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
    if (typeof backend.sessionLiveness === 'function') this.liveness = new SessionLiveness(this, backend);
    this.backendChanged(backend.state);
  }

  observe(repo) {
    const key = repo.rootUri.toString();
    if (this.listeners.has(key)) return;
    this.repositoryRevisions.set(key, (this.repositoryRevisions.get(key) || 0) + 1);
    this.repositoryIdentities.set(key, directoryIdentity(repo.rootUri.fsPath));
    const token = ++this.nextRepositoryToken;
    this.repositoryTokens.set(key, token);
    this.listeners.set(key, repo.state.onDidChange(() => {
      if (!this.isCurrentRepository(repo, token)) return;
      this.groupCache.delete(key);
      if (this.isVisible(repo)) this.scheduleChanged();
    }));
  }

  unobserve(repo) {
    const key = repo.rootUri.toString();
    if (!this.listeners.has(key)) return;
    this.repositoryRevisions.set(key, (this.repositoryRevisions.get(key) || 0) + 1);
    this.listeners.get(key)?.dispose();
    this.listeners.delete(key);
    this.repositoryIdentities.delete(key);
    this.repositoryTokens.delete(key);
    this.sortGeneration++;
    this.sortRanks.delete(key);
    this.groupCache.delete(key);
    this.failedStatuses.delete(key);
    this.statusErrors.delete(key);
    this.backendStatuses.delete(key);
    this.backendErrors.delete(key);
  }

  backendChanged(state) {
    if (this.disposed) return;
    if (state.livenessChanged) {
      this.liveness?.restart();
      this.scheduleChanged();
      return;
    }
    const wasUnavailable = this.backendError !== undefined;
    if (state.status === 'initializing') {
      this.backendGeneration++;
      this.backendStatuses.clear();
      this.backendErrors.clear();
      this.backendError = undefined;
    } else if (state.status === 'error') {
      this.backendError = `Gitpane backend unavailable: ${state.error?.message || 'Unknown error'}`;
      if (['service_exited', 'spawn_error', 'io_error', 'protocol_error', 'response_too_large', 'unsupported_version', 'invalid_backend_path'].includes(state.error?.code)) {
        this.backendGeneration++;
        this.backendStatuses.clear();
      }
    } else if (state.status === 'ready') {
      this.backendError = undefined;
      if (wasUnavailable && !this.busy) this.scheduleRefresh();
    }
    this.scheduleChanged();
  }

  async refreshBackend(repo, cycle, includeWorktrees) {
    const key = repo.rootUri.toString();
    const generation = cycle.generation;
    const current = () => !this.disposed && generation === this.backendGeneration &&
      this.isCurrentRepository(repo, cycle.tokens.get(key));
    if (!current()) return;
    try {
      const watchedPaths = [...new Set(this.watchedPathsFor(repo.rootUri.fsPath))];
      const watchTokens = new Map(watchedPaths.map(value => [value, this.watchTokenFor(repo.rootUri.fsPath, value)]));
      if (watchedPaths.length > 256 || !watchedPaths.every(relativeFile)) throw new Error('Invalid symlink paths to watch.');
      const status = await this.backend.status(repo.rootUri.fsPath, includeWorktrees, { summaryOnly: true, watchedPaths });
      if (!current()) return;
      if (!validBackendStatus(status) || status.files.some(file => !file || !relativeFile(file.path) || !watchedPaths.includes(file.path)) ||
          status.watched_errors.some(error => !error || !relativeFile(error.path) || !watchedPaths.includes(error.path) ||
            typeof error.code !== 'string' || typeof error.message !== 'string' || status.files.some(file => file.path === error.path)) ||
          new Set(status.watched_errors.map(error => error.path)).size !== status.watched_errors.length) {
        throw new Error('Gitpane backend returned an invalid compact repository status. Update the backend and try again.');
      }
      const previous = this.backendStatuses.get(key);
      const preserveWorktrees = !includeWorktrees && previous?.worktrees.length &&
        sameIdentity(directoryIdentity(previous.common_dir), directoryIdentity(status.common_dir));
      const stored = { ...status, watchedPaths, watchTokens, ...(preserveWorktrees ? { worktrees: previous.worktrees } : {}) };
      this.backendStatuses.set(key, stored);
      this.backendErrors.delete(key);
      if (includeWorktrees) await this.openWorkspaceWorktrees(repo, stored, generation, cycle);
    } catch (error) {
      if (!current() || error.code === 'workspace_changed') return;
      this.backendStatuses.delete(key);
      const message = `Could not load Gitpane status for ${repo.rootUri.fsPath}: ${error.message}`;
      if (this.backendErrors.get(key) !== message) this.output.appendLine(message);
      this.backendErrors.set(key, message);
    }
  }

  async refreshRepositories(repos, cycle, visibleOnly) {
    const current = repo => !this.disposed && cycle.generation === this.backendGeneration &&
      (!visibleOnly || this.visible) && this.isCurrentRepository(repo, cycle.tokens.get(repo.rootUri.toString()));
    await parallel(repos, 4, async repo => {
      if (!current(repo)) return;
      try {
        await repo.status();
        if (current(repo)) this.statusSucceeded(repo);
      } catch (error) {
        if (current(repo)) this.statusFailed(repo, error);
      }
      if (current(repo)) await this.refreshBackend(repo, cycle, false);
    });
    if (this.disposed || cycle.generation !== this.backendGeneration || (visibleOnly && !this.visible)) return;
    const commonIdentities = new Map();
    const groups = new Map();
    for (const repo of repos) {
      if (!current(repo)) continue;
      const key = repo.rootUri.toString();
      const status = this.backendStatuses.get(key);
      if (!status) continue;
      if (!commonIdentities.has(status.common_dir)) commonIdentities.set(status.common_dir, directoryIdentity(status.common_dir));
      const common = commonIdentities.get(status.common_dir);
      if (!common) {
        this.backendStatuses.delete(key);
        const message = `Could not load Gitpane status for ${repo.rootUri.fsPath}: Git common directory is unavailable.`;
        if (this.backendErrors.get(key) !== message) this.output.appendLine(message);
        this.backendErrors.set(key, message);
        continue;
      }
      const groupKey = identityKey(common);
      if (!groups.has(groupKey)) groups.set(groupKey, []);
      groups.get(groupKey).push(repo);
    }
    const anchors = [...groups.values()].map(group => group.sort(compareWorktreeAnchors)[0]);
    await parallel(anchors, 4, async repo => {
      if (current(repo)) await this.refreshBackend(repo, cycle, true);
    });
  }

  async openWorkspaceWorktrees(parent, status, generation, cycle) {
    if (typeof this.git.openRepository !== 'function') return;
    const current = () => !this.disposed && generation === this.backendGeneration &&
      this.isCurrentRepository(parent, cycle.tokens.get(parent.rootUri.toString()));
    const common = typeof status.common_dir === 'string' && path.isAbsolute(status.common_dir)
      ? directoryIdentity(status.common_dir) : undefined;
    const groupKey = identityKey(common) || parent.rootUri.toString();
    if (cycle.groups.has(groupKey)) return cycle.groups.get(groupKey);
    const discovery = parallel(status.worktrees, 4, async item => {
      if (!current() ||
          this.backendStatuses.get(parent.rootUri.toString()) !== status || !this.isPathInWorkspace(item.path)) return;
      const identity = directoryIdentity(item.path);
      if (!identity || cycle.visible.has(identityKey(identity))) return;
      const key = identity.path;
      if (!this.worktreeOpenings.has(key)) {
        const opening = (async () => {
          if (!current() ||
              this.backendStatuses.get(parent.rootUri.toString()) !== status ||
              !sameIdentity(identity, directoryIdentity(item.path)) || !this.isPathInWorkspace(item.path)) return;
          try {
            const opened = await this.git.openRepository(vscode.Uri.file(item.path));
            if (!opened || !current() || this.backendStatuses.get(parent.rootUri.toString()) !== status ||
                !this.isPathInWorkspace(item.path) ||
                !sameIdentity(identity, directoryIdentity(item.path))) return;
            if (this.git.repositories.some(repo => repo.rootUri.toString() === opened.rootUri.toString())) {
              this.observe(opened);
              if (this.isVisible(opened)) cycle.visible.add(identityKey(identity));
            }
          } catch (error) {
            if (current()) this.output.appendLine(`Could not open workspace worktree ${item.path}: ${error.message}`);
          }
        })();
        this.worktreeOpenings.set(key, opening);
        opening.finally(() => this.worktreeOpenings.delete(key));
      }
      await this.worktreeOpenings.get(key);
    });
    cycle.groups.set(groupKey, discovery);
    await discovery;
  }

  workspaceView() {
    const identities = new Map();
    const identify = value => {
      if (!identities.has(value)) identities.set(value, directoryIdentity(value));
      return identities.get(value);
    };
    const roots = [...this.workspaceIdentities].flatMap(([source, identity]) =>
      sameIdentity(identity, identify(source)) ? [identity] : []);
    const insideWorkspace = identity => !!identity && roots.some(root => inside(root.path, identity.path));
    const records = [];
    const byIdentity = new Map();
    const groups = new Map();
    for (const repo of this.git.repositories) {
      if (repo.rootUri.scheme !== 'file') continue;
      const key = repo.rootUri.toString();
      const identity = identify(repo.rootUri.fsPath);
      if (!sameIdentity(identity, this.repositoryIdentities.get(key)) || !insideWorkspace(identity)) continue;
      const status = this.backendStatuses.get(key);
      const common = typeof status?.common_dir === 'string' && path.isAbsolute(status.common_dir)
        ? identify(status.common_dir) : undefined;
      const explicit = roots.some(root => sameIdentity(root, identity));
      const record = { repo, key, identity, status, common, explicit };
      records.push(record);
      if (!byIdentity.has(identityKey(identity))) byIdentity.set(identityKey(identity), record);
      if (common) {
        const key = identityKey(common);
        if (!groups.has(key)) groups.set(key, []);
        groups.get(key).push(record);
      }
    }
    for (const group of groups.values()) group.sort((a, b) => compareWorktreeAnchors(a.repo, b.repo));
    const worktrees = new Map();
    const children = new Map();
    const nested = new Set();
    for (const record of records) {
      if (!record.status?.worktrees.length || (record.common && groups.get(identityKey(record.common))[0] !== record)) continue;
      const seen = new Set();
      for (const item of record.status.worktrees) {
        const identity = identify(item.path);
        const key = identityKey(identity);
        if (!insideWorkspace(identity) || sameIdentity(identity, record.identity) || seen.has(key)) continue;
        seen.add(key);
        const candidate = byIdentity.get(key);
        const child = candidate && sameIdentity(record.common, candidate.common) ? candidate : undefined;
        if (child?.explicit) continue;
        if (!worktrees.has(record.key)) worktrees.set(record.key, []);
        worktrees.get(record.key).push(item);
        if (child) {
          if (!children.has(record.key)) children.set(record.key, new Map());
          children.get(record.key).set(item.path, child.repo);
          nested.add(child.key);
        }
      }
    }
    return { records, worktrees, children, nested };
  }

  presentationView() {
    const view = this.workspaceView();
    if (!this.hiddenRepositories) return view;
    const verifiedToken = value => typeof value === 'string' && /^[a-f0-9]{64}$/.test(value) ? value : undefined;
    const identities = new Map();
    for (const items of view.worktrees.values()) for (const item of items) {
      const identity = directoryIdentity(item.path);
      const token = verifiedToken(item.repo_identity);
      if (identity && token) identities.set(identityKey(identity), token);
    }
    let pendingHidden = false;
    const records = view.records.filter(record => {
      const token = verifiedToken(record.status?.repo_identity) || identities.get(identityKey(record.identity));
      const hidden = this.hiddenRepositories.isHidden(record.identity, token);
      if (hidden && token === undefined) pendingHidden = true;
      return !hidden;
    });
    const visible = new Set(records.map(record => record.key));
    const worktrees = new Map();
    const nested = new Set();
    for (const record of records) {
      const items = (view.worktrees.get(record.key) || []).filter(item => {
        const token = verifiedToken(item.repo_identity);
        const hidden = this.hiddenRepositories.isHidden(directoryIdentity(item.path), token);
        if (hidden && token === undefined) pendingHidden = true;
        return !hidden;
      });
      worktrees.set(record.key, items);
      for (const item of items) {
        const child = view.children.get(record.key)?.get(item.path);
        if (child && visible.has(child.rootUri.toString())) nested.add(child.rootUri.toString());
      }
    }
    return { ...view, records, worktrees, nested, pendingHidden };
  }

  directoryIdentity(value) { return directoryIdentity(value); }

  worktreeRepository(parent, worktreePath, view = this.workspaceView()) {
    return view.children.get(parent.rootUri.toString())?.get(worktreePath);
  }

  workspaceWorktrees(parent, view = this.workspaceView()) {
    return view.worktrees.get(parent.rootUri.toString()) || [];
  }

  explicitWorktreeAnchor(repo, view = this.workspaceView()) {
    const target = view.records.find(record => record.repo.rootUri.toString() === repo.rootUri.toString());
    if (!target?.explicit || !target.common) return undefined;
    const anchors = view.records.filter(record => record !== target && sameIdentity(record.common, target.common) &&
      Array.isArray(record.status?.worktrees)).sort((a, b) => compareWorktreeAnchors(a.repo, b.repo));
    for (const anchor of anchors) {
      const item = anchor.status.worktrees.find(worktree => typeof worktree.path === 'string' &&
        typeof worktree.branch === 'string' && sameIdentity(directoryIdentity(worktree.path), target.identity));
      if (item) return { anchor: anchor.repo, path: item.path, branch: item.branch,
        commonIdentity: identityKey(target.common) };
    }
    return undefined;
  }

  sessionTargets(view = this.workspaceView()) {
    const entries = new Map();
    const add = (identity, reference) => {
      if (!identity) return;
      if (!entries.has(identity.path)) entries.set(identity.path, []);
      entries.get(identity.path).push(reference);
    };
    for (const record of view.records) {
      add(record.identity, ['repo', record.key, this.repositoryToken(record.repo), identityKey(record.identity)]);
      for (const worktree of view.worktrees.get(record.key) || []) {
        const identity = directoryIdentity(worktree.path);
        add(identity, ['worktree', record.key, this.repositoryToken(record.repo), worktree.path, worktree.branch, identityKey(identity)]);
      }
    }
    return [...entries].sort(([a], [b]) => a.localeCompare(b)).map(([target, references]) => ({
      path: target, references: references.map(value => JSON.stringify(value)).sort()
    }));
  }

  fetchTargets(view = this.workspaceView()) {
    const entries = new Map();
    for (const record of view.records) {
      const commonDir = record.status?.common_dir;
      const key = identityKey(record.identity);
      entries.set(key, { path: record.identity.path, commonDir, repoIdentity: record.status?.repo_identity,
        commonIdentity: identityKey(record.common),
        guard: JSON.stringify(['repo', record.key, this.repositoryToken(record.repo), key,
          identityKey(record.common), record.status?.repo_identity, record.status?.branch]) });
    }
    for (const record of view.records) {
      for (const worktree of view.worktrees.get(record.key) || []) {
        const identity = directoryIdentity(worktree.path);
        const key = identityKey(identity);
        if (!identity || entries.has(key)) continue;
        entries.set(key, { path: identity.path, commonDir: record.status?.common_dir, repoIdentity: worktree.repo_identity,
          commonIdentity: identityKey(record.common),
          guard: JSON.stringify(['worktree', record.key, this.repositoryToken(record.repo), key,
            identityKey(record.common), worktree.repo_identity, worktree.path, worktree.branch]) });
      }
    }
    return [...entries.values()].sort((a, b) => a.path.localeCompare(b.path));
  }

  sessionLive(value) {
    const identity = directoryIdentity(value);
    return !!identity && this.liveness?.live.get(identity.path) === true;
  }

  changed() {
    if (this.disposed) return;
    this.liveness?.changed();
    if (this.sortOrder !== 'workspace') this.scheduleSort();
    this.emitter.fire();
  }

  setSortOrder(order) {
    if (!SORT_ORDERS.has(order) || order === this.sortOrder) return;
    this.sortOrder = order;
    this.sortRanks.clear();
    this.sortError = undefined;
    this.sortGeneration++;
    this.changed();
  }

  scheduleSort() {
    this.sortGeneration++;
    if (this.sortTimer !== undefined) return;
    this.sortTimer = setTimeout(() => {
      this.sortTimer = undefined;
      this.refreshSort();
    }, 0);
  }

  async refreshSort() {
    if (this.disposed || this.sortOrder === 'workspace') return;
    const generation = this.sortGeneration;
    const order = this.sortOrder;
    const nodes = this.getChildren().filter(node => node.type === 'repo');
    const tokens = nodes.map(node => this.repositoryToken(node.repo));
    const current = () => !this.disposed && generation === this.sortGeneration && order === this.sortOrder &&
      nodes.every((node, index) => this.isCurrentRepository(node.repo, tokens[index]));
    try {
      const repositories = nodes.map(({ repo }) => {
        const identity = this.repositoryIdentities.get(repo.rootUri.toString());
        if (!identity) throw new Error('Repository identity is no longer available.');
        const roots = [...this.workspaceIdentities.values()].filter(root => root && inside(root.path, identity.path));
        roots.sort((a, b) => b.path.length - a.path.length);
        const display = roots.length ? path.relative(roots[0].path, identity.path) || path.basename(identity.path) : path.basename(identity.path);
        return { path: repo.rootUri.fsPath, display, dirty: this.groupsFor(repo).length > 0 || !!this.backendStatuses.get(repo.rootUri.toString())?.is_dirty };
      });
      const result = await this.backend.sortRepositories(order, repositories);
      if (!current()) return;
      const indices = result?.indices;
      if (!Array.isArray(indices) || indices.length !== nodes.length || new Set(indices).size !== nodes.length ||
          indices.some(index => !Number.isSafeInteger(index) || index < 0 || index >= nodes.length) ||
          nodes.some((node, index) => !this.isCurrentRepository(node.repo, tokens[index]))) throw new Error('Gitpane returned an invalid repository order.');
      this.sortRanks = new Map(indices.map((index, rank) => [nodes[index].repo.rootUri.toString(), rank]));
      this.sortError = undefined;
      this.emitter.fire();
    } catch (error) {
      if (!current()) return;
      this.sortError = `Could not sort repositories: ${error.message}`;
      this.output.appendLine(this.sortError);
      this.emitter.fire();
    }
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

  updateWorkspacePaths() {
    const folders = (vscode.workspace.workspaceFolders || []).filter(folder => folder.uri.scheme === 'file');
    const active = new Set(folders.map(folder => folder.uri.fsPath));
    if (!this.sharedWorkspaceIdentities) {
      for (const key of this.workspaceIdentities.keys()) if (!active.has(key)) this.workspaceIdentities.delete(key);
      for (const folder of folders) {
        const key = folder.uri.fsPath;
        if (!this.workspaceIdentities.has(key)) this.workspaceIdentities.set(key, directoryIdentity(key));
      }
    }
    this.workspacePaths = folders.map(folder => folder.uri.fsPath);
  }

  isPathInWorkspace(value) {
    const target = directoryIdentity(value);
    return this.containsIdentity(target);
  }

  containsIdentity(target) {
    if (!target) return false;
    for (const [source, identity] of this.workspaceIdentities) {
      if (sameIdentity(identity, directoryIdentity(source)) && inside(identity.path, target.path)) return true;
    }
    return false;
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
    this.liveness?.setVisible(visible);
    this.pins?.setVisible(visible);
    this.workspaceRepositories?.setVisible(visible);
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
    const cycle = { generation: this.backendGeneration,
      tokens: new Map(repos.map(repo => [repo.rootUri.toString(), this.repositoryToken(repo)])),
      visible: new Set(repos.map(repo => identityKey(this.repositoryIdentities.get(repo.rootUri.toString())))), groups: new Map() };
    await this.refreshRepositories(repos, cycle, true);
    this.pollError = undefined;
    this.scheduleChanged();
  }

  isVisible(repo) {
    if (!repo || repo.rootUri.scheme !== 'file') return false;
    const identity = this.repositoryIdentities.get(repo.rootUri.toString());
    const current = directoryIdentity(repo.rootUri.fsPath);
    return sameIdentity(identity, current) && this.containsIdentity(current);
  }

  repositoryToken(repo) {
    return repo?.rootUri && this.repositoryTokens.get(repo.rootUri.toString());
  }

  repositoryRevision(uri) {
    return this.repositoryRevisions.get(uri.toString()) || 0;
  }

  isCurrentRepository(repo, token) {
    return !this.disposed && token !== undefined && this.repositoryToken(repo) === token && this.isVisible(repo);
  }

  refresh(retryBackend = false) {
    if (this.disposed) return Promise.resolve();
    return new Promise((resolve, reject) => {
      const waiter = {
        resolve: () => { this.allRefreshWaiters.delete(waiter); resolve(); },
        reject: error => { this.allRefreshWaiters.delete(waiter); reject(error); }
      };
      this.allRefreshWaiters.add(waiter);
      this.runRefresh(retryBackend).then(waiter.resolve, waiter.reject);
    });
  }

  async runRefresh(retryBackend = false) {
    if (retryBackend && this.backend.state.status === 'error') {
      try { await this.backend.retry(); }
      catch { this.changed(); return; }
    }
    if (this.pollPromise) await this.pollPromise;
    if (this.disposed) return;
    if (this.busy) {
      this.pending = true;
      return new Promise((resolve, reject) => {
        const waiter = {
          resolve: () => { this.allRefreshWaiters.delete(waiter); resolve(); },
          reject: error => { this.allRefreshWaiters.delete(waiter); reject(error); }
        };
        this.allRefreshWaiters.add(waiter);
        this.refreshWaiters.push(waiter);
      });
    }
    this.busy = true;
    this.pollError = undefined;
    this.updateWorkspacePaths();
    this.changed();
    try {
      await this.backend.syncWorkspaceRoots?.();
      const repos = this.git.repositories.filter(repo => this.isVisible(repo));
      const cycle = { generation: this.backendGeneration,
        tokens: new Map(repos.map(repo => [repo.rootUri.toString(), this.repositoryToken(repo)])),
        visible: new Set(repos.map(repo => identityKey(this.repositoryIdentities.get(repo.rootUri.toString())))), groups: new Map() };
      await this.refreshRepositories(repos, cycle, false);
      const active = new Set(this.git.repositories.filter(repo => this.isVisible(repo)).map(repo => repo.rootUri.toString()));
      for (const key of this.statusErrors.keys()) {
        if (!active.has(key)) {
          this.statusErrors.delete(key);
          this.failedStatuses.delete(key);
        }
      }
      for (const key of this.backendStatuses.keys()) if (!active.has(key)) this.backendStatuses.delete(key);
      for (const key of this.backendErrors.keys()) if (!active.has(key)) this.backendErrors.delete(key);
    } finally {
      this.busy = false;
      this.changed();
      if (this.pending && !this.disposed) {
        this.pending = false;
        const waiters = this.refreshWaiters.splice(0);
        this.refresh().then(
          () => { for (const waiter of waiters) waiter.resolve(); },
          error => {
            for (const waiter of waiters) waiter.reject(error);
            if (!waiters.length) {
              this.fail(`Could not refresh repository status: ${error.message}`);
              this.changed();
            }
          }
        );
      }
    }
  }

  getChildren(node, workspaceView) {
    if (!node) {
      const missingFolders = this.workspacePaths.flatMap(folder => {
        const current = directoryIdentity(folder);
        if (!current) return [`Workspace folder unavailable: ${folder}`];
        if (!sameIdentity(this.workspaceIdentities.get(folder), current)) {
          return [`Workspace folder changed: ${folder}. Remove and re-add it to the workspace.`];
        }
        return [];
      });
      const failures = [this.pollError, this.backendError, this.sortError, this.liveness?.error, this.pins?.error, this.workspaceRepositories?.error, this.hiddenRepositories?.error, ...this.statusErrors.values(), ...this.backendErrors.values(), ...missingFolders].filter(Boolean).map(message => ({ type: 'error', message }));
      const view = workspaceView || this.presentationView();
      if (view.pendingHidden) failures.push({ type: 'error', message: 'Hidden repository identity verification is pending.' });
      let repos = view.records.filter(record => !view.nested.has(record.key)).map(({ repo }) => ({
        repo, dirty: this.groupsFor(repo).length > 0 || !!this.backendStatuses.get(repo.rootUri.toString())?.is_dirty
      })).sort((a, b) => Number(b.dirty) - Number(a.dirty) || a.repo.rootUri.fsPath.localeCompare(b.repo.rootUri.fsPath));
      if (this.sortOrder !== 'workspace' && this.sortRanks.size) {
        repos.sort((a, b) => (this.sortRanks.get(a.repo.rootUri.toString()) ?? Number.MAX_SAFE_INTEGER) -
          (this.sortRanks.get(b.repo.rootUri.toString()) ?? Number.MAX_SAFE_INTEGER));
      }
      if (this.busy && !repos.length && !failures.length) return [{ type: 'info', message: 'Loading repositories…' }];
      if (!repos.length && !failures.length) return [{ type: 'info', message: this.hiddenRepositories?.repositories.length && this.workspaceView().records.length
        ? 'Repositories are hidden. Use Restore Hidden Repositories to show them.' : this.workspacePaths.length
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
    this.liveness?.dispose();
    for (const waiter of this.allRefreshWaiters) waiter.resolve();
    this.refreshWaiters = [];
    if (this.changeTimer !== undefined) clearTimeout(this.changeTimer);
    if (this.refreshTimer !== undefined) clearTimeout(this.refreshTimer);
    if (this.pollTimer !== undefined) clearTimeout(this.pollTimer);
    if (this.sortTimer !== undefined) clearTimeout(this.sortTimer);
    for (const subscription of this.subscriptions) subscription.dispose();
    for (const subscription of this.listeners.values()) subscription.dispose();
    this.emitter.dispose();
  }
}

module.exports = { RepositoryTree };
