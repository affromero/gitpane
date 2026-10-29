'use strict';

const { statusRevision } = require('./link-documents');

const fs = require('node:fs');
const path = require('node:path');
const { isSubmodule, richComparison } = require('./model');
const { scopedFile, validRecord, validatePage } = require('./change-pager');

function safeRelative(value) {
  return typeof value === 'string' && !!value && !path.isAbsolute(value) &&
    !value.split(process.platform === 'win32' ? /[\\/]/ : /\//).some(part => !part || part === '.' || part === '..');
}

class SubmoduleProvider {
  constructor(tree, backend, git, options = {}) {
    this.tree = tree;
    this.backend = backend;
    this.git = git;
    this.vscode = options.vscode || require('vscode');
    this.resolveChange = options.resolveChange;
    this.openGraph = options.openGraph;
    this.linkDocuments = options.linkDocuments;
    this.generation = 0;
    this.disposed = false;
    this.graphWatch = undefined;
    this.graphSubscription = tree.onDidChangeTreeData?.(() => this.scheduleGraphCheck());
  }

  current(node, generation, token) {
    if (this.disposed || generation !== this.generation || !this.tree.isCurrentRepository(node?.repo, token)) return undefined;
    const now = this.resolveChange(node);
    if (!now || !this.tree.isVisible(now.repo) || now.group !== node.group ||
      now.change.status !== node.change.status || now.change.uri.toString() !== node.change.uri.toString()) return undefined;
    return now;
  }

  childCurrent(node, generation, childPath, parentToken, childRepo, childToken, persistent = false) {
    if (this.disposed || (!persistent && generation !== this.generation) ||
        !this.tree.isCurrentRepository(node?.repo, parentToken) || !this.tree.isPathInWorkspace(childPath)) return false;
    if (node.parentWorktree) {
      const parent = this.git.repositories.find(repo => repo.rootUri.toString() === node.parentWorktree.repoKey);
      if (!parent || !this.tree.isCurrentRepository(parent, node.parentWorktree.repoToken) ||
          !this.tree.workspaceWorktrees(parent).some(item => item.path === node.parentWorktree.path) ||
          this.tree.worktreeRepository(parent, node.parentWorktree.path)?.rootUri.toString() !== node.repo.rootUri.toString()) return false;
    }
    // Legacy callers do not have a canonical record to verify independently.
    if (!node.record?.path && (persistent ? !this.resolveChange(node) : !this.current(node, generation, parentToken))) return false;
    try {
      const original = fs.realpathSync.native(node.change.uri.fsPath);
      const resolved = fs.realpathSync.native(childPath);
      if (original !== resolved) return false;
      return !childRepo || this.tree.isCurrentRepository(childRepo, childToken);
    } catch { return false; }
  }

  clearGraphWatch() {
    const watch = this.graphWatch;
    this.graphWatch = undefined;
    if (watch) watch.valid = false;
    if (watch?.timer !== undefined) clearTimeout(watch.timer);
  }

  scheduleGraphCheck() {
    const watch = this.graphWatch;
    if (!watch?.valid || watch.timer !== undefined) return;
    if (watch.pending) { watch.again = true; return; }
    const delay = Math.max(500, (watch.retryAt || 0) - Date.now());
    watch.timer = setTimeout(async () => {
      watch.timer = undefined;
      if (this.graphWatch !== watch || !watch.valid) return;
      watch.pending = true;
      let valid = false;
      let failed = false;
      try {
        if (watch.current()) valid = await watch.validate();
      } catch (error) {
        failed = true;
        watch.retryAt = Date.now() + 5000;
        if (this.graphWatch === watch && !this.disposed) {
          await this.vscode.window.showErrorMessage(`Gitpane submodule: ${error.message}`);
        }
      } finally {
        watch.pending = false;
      }
      if (this.graphWatch !== watch || this.disposed) return;
      if (failed) {
        if (watch.again) { watch.again = false; this.scheduleGraphCheck(); }
        return;
      }
      watch.retryAt = 0;
      if (!valid) {
        watch.valid = false;
        this.tree.changed();
      } else if (watch.again) {
        watch.again = false;
        this.scheduleGraphCheck();
      }
    }, delay);
  }

  async openGuardedGraph(child, node, current, checkAll) {
    this.clearGraphWatch();
    if (typeof node.record?.path !== 'string' || typeof node.record?.group !== 'string') {
      return this.openGraph(child, current);
    }
    const watch = { node, current, checkAll, valid: true, timer: undefined, pending: false, again: false };
    this.graphWatch = watch;
    const validate = () => {
      if (!watch.valid || !current()) return Promise.resolve(false);
      if (watch.validation) return watch.validation;
      const pending = (async () => {
        if (!await checkAll() || !current()) { watch.valid = false; return false; }
        return true;
      })();
      watch.validation = pending;
      pending.finally(() => { if (watch.validation === pending) watch.validation = undefined; }).catch(() => {});
      return pending;
    };
    watch.validate = validate;
    try { return await this.openGraph(child, () => watch.valid && current(), validate); }
    catch (error) { if (this.graphWatch === watch) this.clearGraphWatch(); throw error; }
  }

  async open(selected) {
    this.clearGraphWatch();
    const generation = ++this.generation;
    try {
      await this.openLevel(selected, generation, []);
    } catch (error) {
      await this.vscode.window.showErrorMessage(`Gitpane submodule: ${error.message}`);
    }
  }

  async openLevel(selected, generation, ancestors) {
      const parentToken = selected?.repo && this.tree.repositoryToken(selected.repo);
      const currentNode = ancestors.length ? selected : this.current(selected, generation, parentToken);
      if (!currentNode) throw new Error('The submodule is no longer in the workspace.');
      const node = currentNode.record ? { ...currentNode, record: structuredClone(currentNode.record) } : currentNode;
      if (ancestors.length >= 32) throw new Error('The submodule nesting limit was reached.');
      const checkAncestors = async () => {
        for (const ancestor of ancestors) if (!await ancestor.checkOwn(false)) return false;
        return true;
      };
      if (!await checkAncestors()) throw new Error('The parent submodule changed; refresh and try again.');
      const relative = path.relative(node.repo.rootUri.fsPath, node.change.uri.fsPath);
      if (!safeRelative(relative) || !(node.record?.is_submodule ||
        isSubmodule(node.repo.rootUri.fsPath, node.change.uri.fsPath, node.repo.state.submodules))) {
        throw new Error('The selected change is not a current submodule.');
      }
      const group = node.record?.group || node.group;
      const originalPath = node.record?.kind === 'index_renamed' ? node.record.original_path : undefined;
      if (originalPath !== undefined && !safeRelative(originalPath)) throw new Error('The submodule has an invalid original path.');
      const result = await this.backend.inspectSubmodule(node.repo.rootUri.fsPath, relative, group, originalPath);
      const validOid = value => value === null || (typeof value === 'string' && /^[a-f0-9]{40,64}$/.test(value));
      if (!result || typeof result.repo !== 'string' || !path.isAbsolute(result.repo) ||
        !validOid(result.base_oid) || !validOid(result.head_oid) ||
        typeof result.child_head_oid !== 'string' || !/^[a-f0-9]{40,64}$/.test(result.child_head_oid) ||
        !Array.isArray(result.pointer_files)) {
        throw new Error('Gitpane returned invalid submodule details.');
      }
      if (!await checkAncestors()) throw new Error('The parent submodule changed; refresh and try again.');
      if (!this.childCurrent(node, generation, result.repo, parentToken)) throw new Error('The submodule is no longer in the workspace.');
      const childUri = this.vscode.Uri.file(result.repo);
      const childKey = childUri.toString();
      const revision = this.tree.repositoryRevision(childUri);
      const existingChild = this.git.repositories.find(repo => repo.rootUri.toString() === childKey);
      const existingToken = existingChild && this.tree.repositoryToken(existingChild);
      if (!await checkAncestors() || !this.childCurrent(node, generation, result.repo, parentToken)) {
        throw new Error('The parent submodule changed; refresh and try again.');
      }
      if (ancestors.length) {
        const current = await this.backend.verifyChanges(node.repo.rootUri.fsPath, node.record);
        if (!validRecord(current?.change) || JSON.stringify(current.change) !== JSON.stringify(node.record) ||
            !ancestors.every(ancestor => ancestor.current()) || !this.childCurrent(node, generation, result.repo, parentToken)) {
          throw new Error('The submodule changed; refresh and try again.');
        }
      }
      const opened = await this.git.openRepository(childUri);
      const expectedRevision = revision + (existingToken === undefined ? 1 : 0);
      const child = this.git.repositories.find(repo => repo.rootUri.toString() === childKey);
      const childToken = child && this.tree.repositoryToken(child);
      if (!opened || opened.rootUri.toString() !== childKey || !child ||
          this.tree.repositoryRevision(childUri) !== expectedRevision ||
          (existingToken !== undefined && existingToken !== childToken) ||
          !this.childCurrent(node, generation, result.repo, parentToken, child, childToken) || !await checkAncestors()) {
        throw new Error('The submodule is unavailable or no longer in the workspace.');
      }
      const fixedChoices = [{ label: 'Open Commit Graph', kind: 'graph' }];
      for (const file of result.pointer_files) {
        if (!file || !safeRelative(file.path) || !['A', 'D', 'M', 'R', '?'].includes(file.status) ||
          (file.status === 'R' && !safeRelative(file.old_path)) || typeof file.is_submodule !== 'boolean') {
          throw new Error('Gitpane returned an invalid submodule change.');
        }
        fixedChoices.push({ label: file.path, description: `Committed ${file.status}`, kind: 'pointer', file });
      }
      const ownCurrent = persistent => this.childCurrent(node, generation, result.repo, parentToken, child, childToken, persistent);
      const currentChild = () => ancestors.every(ancestor => ancestor.current()) && ownCurrent(false);
      const currentLink = () => ancestors.every(ancestor => ancestor.currentLink()) && ownCurrent(true);
      const inspectCurrent = async () => {
        const latest = await this.backend.inspectSubmodule(node.repo.rootUri.fsPath, relative, group, originalPath);
        return latest?.repo === result.repo && latest?.base_oid === result.base_oid &&
          latest?.head_oid === result.head_oid && latest?.child_head_oid === result.child_head_oid;
      };
      const checkOwn = async persistent => {
        const current = persistent ? currentLink : currentChild;
        if (!current()) return false;
        if (typeof node.record?.path === 'string' && typeof node.record?.group === 'string') {
          const parent = await this.backend.verifyChanges(node.repo.rootUri.fsPath, node.record);
          if (!current() || !validRecord(parent?.change) || JSON.stringify(parent.change) !== JSON.stringify(node.record)) return false;
        }
        const status = await this.backend.status(child.rootUri.fsPath, false, { summaryOnly: true, watchedPaths: [] });
        if (!current() || status?.head_oid !== result.child_head_oid) return false;
        return await inspectCurrent() && current();
      };
      const checkHead = async (persistent = false) => {
        for (const ancestor of ancestors) if (!await ancestor.checkOwn(persistent)) return false;
        return checkOwn(persistent);
      };
      if (!await checkHead()) throw new Error('The submodule HEAD changed; refresh and try again.');
      const initialChoices = [...fixedChoices, { label: 'Browse working changes', kind: 'browse' }];
      const initial = await this.vscode.window.showQuickPick(initialChoices, { placeHolder: `Inspect ${relative}`, matchOnDescription: true });
      if (!initial || !initialChoices.includes(initial)) return;
      if (!await checkHead()) throw new Error('The submodule changed; refresh and try again.');
      if (initial.kind === 'graph') return await this.openGuardedGraph(child, node, currentChild, checkHead);
      if (initial.kind === 'pointer') {
        if (!await checkHead()) throw new Error('The submodule changed; refresh and try again.');
        return await this.openPointer(child, result, initial.file);
      }
      const session = await this.backend.openChangesSession(child.rootUri.fsPath);
      let sessionClosed = false;
      try {
        let page = validatePage(await session.begin(), undefined, 0);
        let chosen;
        for (;;) {
          if (!await checkHead()) throw new Error('The submodule changed; refresh and try again.');
          const labels = { merge: 'Conflicts', index: 'Staged', working: 'Unstaged', untracked: 'Untracked' };
          const choices = [...fixedChoices, ...page.changes.map(record => ({
            label: record.kind === 'untracked_directory' ? `${record.path}/` : record.path,
            description: labels[record.group], kind: 'change', record
          }))];
          if (page.previous_cursor !== null) choices.push({ label: 'Previous changes', kind: 'previous' });
          if (page.next_cursor !== null) choices.push({ label: 'Next changes', kind: 'next' });
          chosen = await this.vscode.window.showQuickPick(choices, { placeHolder: `Inspect ${relative}`, matchOnDescription: true });
          if (!chosen || !choices.includes(chosen)) return;
          if (!await checkHead()) throw new Error('The submodule changed; refresh and try again.');
          if (!['previous', 'next'].includes(chosen.kind)) break;
          const cursor = chosen.kind === 'previous' ? page.previous_cursor : page.next_cursor;
          const next = validatePage(await session.next(page.snapshot, cursor), page.snapshot, cursor);
          if (next.revision !== page.revision) throw new Error('The submodule changed; refresh and try again.');
          page = next;
        }
        if (chosen.kind === 'graph') return await this.openGuardedGraph(child, node, currentChild, checkHead);
        if (chosen.kind === 'pointer') {
          if (!await checkHead()) throw new Error('The submodule changed; refresh and try again.');
          return await this.openPointer(child, result, chosen.file);
        }
        const record = structuredClone(chosen.record);
        if (record.kind === 'untracked_directory') throw new Error('Open the nested repository to inspect its changes.');
        const checked = await this.backend.verifyChanges(child.rootUri.fsPath, record);
        if (!await checkHead() || !validRecord(checked?.change) || JSON.stringify(checked.change) !== JSON.stringify(record)) {
          throw new Error('The submodule change is no longer available.');
        }
        const uri = this.vscode.Uri.file(scopedFile(child.rootUri.fsPath, record.path));
        if (record.is_submodule || isSubmodule(child.rootUri.fsPath, uri.fsPath, child.state.submodules)) {
          session.close();
          sessionClosed = true;
          const nested = { repo: child, group: record.group, record, change: { uri } };
          return await this.openLevel(nested, generation, [...ancestors,
            { current: () => ownCurrent(false), currentLink: () => ownCurrent(true), checkOwn }]);
        }
        const plan = richComparison(record);
        const empty = uri.with({ scheme: 'gitpane-empty', query: '' });
        const resolve = async side => {
          if (side.empty) return empty;
          const target = scopedFile(child.rootUri.fsPath, side.filePath || side.gitPath);
          const sideUri = this.vscode.Uri.file(target);
          if (!side.filePath) return this.git.toGitUri(sideUri, side.ref);
          const stat = fs.lstatSync(target);
          if (!stat.isSymbolicLink()) {
            if (!stat.isFile()) throw new Error('The changed file is unavailable.');
            return sideUri;
          }
          if (!this.linkDocuments) throw new Error('Symlink diffs are unavailable.');
          const verify = async () => {
            if (!await checkHead(true)) return false;
            const current = await this.backend.verifyChanges(child.rootUri.fsPath, record);
            return JSON.stringify(current?.change) === JSON.stringify(record);
          };
          const revision = statusRevision(() => this.tree.backendStatuses?.get(childKey), side.filePath,
            () => child.state?.HEAD?.commit);
          return this.linkDocuments.create(sideUri, child.rootUri.fsPath, side.filePath, currentLink, verify, revision);
        };
        const left = await resolve(plan.left);
        const right = await resolve(plan.right);
        if (!await checkHead()) throw new Error('The submodule changed; refresh and try again.');
        const finalRecord = await this.backend.verifyChanges(child.rootUri.fsPath, record);
        if (!validRecord(finalRecord?.change) || JSON.stringify(finalRecord.change) !== JSON.stringify(record) || !currentChild()) {
          throw new Error('The submodule changed; refresh and try again.');
        }
        await this.vscode.commands.executeCommand('vscode.diff', left, right, `${path.basename(uri.fsPath)} (${record.group})`);
      } finally { if (!sessionClosed) session.close(); }
  }

  async openPointer(repo, result, file) {
    if (file.is_submodule) throw new Error('Nested submodule changes cannot be opened here.');
    const uri = this.vscode.Uri.file(path.join(repo.rootUri.fsPath, file.path));
    const old = file.status === 'R' ? this.vscode.Uri.file(path.join(repo.rootUri.fsPath, file.old_path)) : uri;
    const empty = uri.with({ scheme: 'gitpane-empty', query: '' });
    const left = file.status === 'A' || !result.base_oid ? empty : this.git.toGitUri(old, result.base_oid);
    const right = file.status === 'D' || !result.head_oid ? empty : this.git.toGitUri(uri, result.head_oid);
    await this.vscode.commands.executeCommand('vscode.diff', left, right, `${path.basename(file.path)} (submodule)`);
  }

  dispose() { this.disposed = true; this.generation++; this.clearGraphWatch(); this.graphSubscription?.dispose(); }
}

module.exports = { SubmoduleProvider };
