'use strict';

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

  childCurrent(node, generation, childPath, parentToken, childRepo, childToken) {
    if (this.disposed || generation !== this.generation ||
        !this.tree.isCurrentRepository(node?.repo, parentToken) || !this.tree.isPathInWorkspace(childPath)) return false;
    if (node.parentWorktree) {
      const parent = this.git.repositories.find(repo => repo.rootUri.toString() === node.parentWorktree.repoKey);
      if (!parent || !this.tree.isCurrentRepository(parent, node.parentWorktree.repoToken) ||
          !this.tree.workspaceWorktrees(parent).some(item => item.path === node.parentWorktree.path) ||
          this.tree.worktreeRepository(parent, node.parentWorktree.path)?.rootUri.toString() !== node.repo.rootUri.toString()) return false;
    }
    // Legacy callers do not have a canonical record to verify independently.
    if (!node.record?.path && !this.current(node, generation, parentToken)) return false;
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
    watch.timer = setTimeout(async () => {
      watch.timer = undefined;
      if (this.graphWatch !== watch || !watch.valid) return;
      watch.pending = true;
      let valid = false;
      try {
        if (watch.current()) {
          const parent = await this.backend.verifyChanges(watch.node.repo.rootUri.fsPath, watch.node.record);
          valid = watch.current() && validRecord(parent?.change) &&
            JSON.stringify(parent.change) === JSON.stringify(watch.node.record);
        }
      } catch (error) {
        if (this.graphWatch === watch && !this.disposed) {
          await this.vscode.window.showErrorMessage(`Gitpane submodule: ${error.message}`);
        }
      } finally {
        watch.pending = false;
      }
      if (this.graphWatch !== watch || this.disposed) return;
      if (!valid) {
        watch.valid = false;
        this.tree.changed();
      } else if (watch.again) {
        watch.again = false;
        this.scheduleGraphCheck();
      }
    }, 500);
  }

  async openGuardedGraph(child, node, current) {
    this.clearGraphWatch();
    if (typeof node.record?.path !== 'string' || typeof node.record?.group !== 'string') {
      return this.openGraph(child, current);
    }
    const watch = { node, current, valid: true, timer: undefined, pending: false, again: false };
    this.graphWatch = watch;
    try { return await this.openGraph(child, () => watch.valid && current()); }
    catch (error) { if (this.graphWatch === watch) this.clearGraphWatch(); throw error; }
  }

  async open(selected) {
    this.clearGraphWatch();
    const generation = ++this.generation;
    const parentToken = selected?.repo && this.tree.repositoryToken(selected.repo);
    try {
      const node = this.current(selected, generation, parentToken);
      if (!node) throw new Error('The submodule is no longer in the workspace.');
      const relative = path.relative(node.repo.rootUri.fsPath, node.change.uri.fsPath);
      if (!safeRelative(relative) || !(node.record?.is_submodule ||
        isSubmodule(node.repo.rootUri.fsPath, node.change.uri.fsPath, node.repo.state.submodules))) {
        throw new Error('The selected change is not a current submodule.');
      }
      const result = await this.backend.inspectSubmodule(node.repo.rootUri.fsPath, relative);
      if (!result || typeof result.repo !== 'string' || !path.isAbsolute(result.repo) ||
        (result.base_oid !== null && !/^[a-f0-9]{40,64}$/.test(result.base_oid)) ||
        !/^[a-f0-9]{40,64}$/.test(result.head_oid) || !Array.isArray(result.pointer_files)) {
        throw new Error('Gitpane returned invalid submodule details.');
      }
      if (!this.childCurrent(node, generation, result.repo, parentToken)) throw new Error('The submodule is no longer in the workspace.');
      const childUri = this.vscode.Uri.file(result.repo);
      const childKey = childUri.toString();
      const revision = this.tree.repositoryRevision(childUri);
      const existingChild = this.git.repositories.find(repo => repo.rootUri.toString() === childKey);
      const existingToken = existingChild && this.tree.repositoryToken(existingChild);
      const opened = await this.git.openRepository(childUri);
      const expectedRevision = revision + (existingToken === undefined ? 1 : 0);
      const child = this.git.repositories.find(repo => repo.rootUri.toString() === childKey);
      const childToken = child && this.tree.repositoryToken(child);
      if (!opened || opened.rootUri.toString() !== childKey || !child ||
          this.tree.repositoryRevision(childUri) !== expectedRevision ||
          (existingToken !== undefined && existingToken !== childToken) ||
          !this.childCurrent(node, generation, result.repo, parentToken, child, childToken)) {
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
      const currentChild = () => this.childCurrent(node, generation, result.repo, parentToken, child, childToken);
      const checkHead = async () => {
        if (!currentChild()) return false;
        if (typeof node.record?.path === 'string' && typeof node.record?.group === 'string') {
          const parent = await this.backend.verifyChanges(node.repo.rootUri.fsPath, node.record);
          if (!currentChild() || !validRecord(parent?.change) || JSON.stringify(parent.change) !== JSON.stringify(node.record)) return false;
        }
        const status = await this.backend.status(child.rootUri.fsPath, false);
        return currentChild() && status?.head_oid === result.head_oid;
      };
      const checkPointer = async () => {
        if (!currentChild()) return false;
        const latest = await this.backend.inspectSubmodule(node.repo.rootUri.fsPath, relative);
        return currentChild() && latest?.repo === result.repo && latest?.base_oid === result.base_oid &&
          latest?.head_oid === result.head_oid;
      };
      if (!await checkHead()) throw new Error('The submodule HEAD changed; refresh and try again.');
      const initialChoices = [...fixedChoices, { label: 'Browse working changes', kind: 'browse' }];
      const initial = await this.vscode.window.showQuickPick(initialChoices, { placeHolder: `Inspect ${relative}`, matchOnDescription: true });
      if (!initial || !initialChoices.includes(initial)) return;
      if (!await checkHead()) throw new Error('The submodule changed; refresh and try again.');
      if (initial.kind === 'graph') return await this.openGuardedGraph(child, node, currentChild);
      if (initial.kind === 'pointer') {
        if (!await checkPointer()) throw new Error('The submodule changed; refresh and try again.');
        return await this.openPointer(child, result, initial.file);
      }
      const session = await this.backend.openChangesSession(child.rootUri.fsPath);
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
        if (chosen.kind === 'graph') return await this.openGuardedGraph(child, node, currentChild);
        if (chosen.kind === 'pointer') {
          if (!await checkPointer()) throw new Error('The submodule changed; refresh and try again.');
          return await this.openPointer(child, result, chosen.file);
        }
        const record = chosen.record;
        if (record.kind === 'untracked_directory') throw new Error('Open the nested repository to inspect its changes.');
        if (record.is_submodule || isSubmodule(child.rootUri.fsPath,
          path.join(child.rootUri.fsPath, ...record.path.split('/')), child.state.submodules)) {
          throw new Error('Nested submodule changes cannot be opened here.');
        }
        const checked = await this.backend.verifyChanges(child.rootUri.fsPath, record);
        if (!await checkHead() || !validRecord(checked?.change) || JSON.stringify(checked.change) !== JSON.stringify(record)) {
          throw new Error('The submodule change is no longer available.');
        }
        const uri = this.vscode.Uri.file(scopedFile(child.rootUri.fsPath, record.path));
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
          const currentLink = () => {
            if (this.disposed || !this.tree.isCurrentRepository(node.repo, parentToken) ||
                !this.tree.isCurrentRepository(child, childToken) || !this.tree.isPathInWorkspace(result.repo)) return false;
            if (node.parentWorktree) {
              const parent = this.git.repositories.find(repo => repo.rootUri.toString() === node.parentWorktree.repoKey);
              if (!parent || !this.tree.isCurrentRepository(parent, node.parentWorktree.repoToken) ||
                  !this.tree.workspaceWorktrees(parent).some(item => item.path === node.parentWorktree.path) ||
                  this.tree.worktreeRepository(parent, node.parentWorktree.path)?.rootUri.toString() !== node.repo.rootUri.toString()) return false;
            }
            try {
              if (fs.realpathSync.native(node.change.uri.fsPath) !== fs.realpathSync.native(result.repo)) return false;
            } catch { return false; }
            return true;
          };
          const verify = async () => {
            const status = await this.backend.status(child.rootUri.fsPath, false);
            if (status?.head_oid !== result.head_oid) return false;
            if (typeof node.record?.path === 'string' && typeof node.record?.group === 'string') {
              const parent = await this.backend.verifyChanges(node.repo.rootUri.fsPath, node.record);
              if (JSON.stringify(parent?.change) !== JSON.stringify(node.record)) return false;
            } else if (!this.resolveChange(node)) return false;
            const current = await this.backend.verifyChanges(child.rootUri.fsPath, record);
            return JSON.stringify(current?.change) === JSON.stringify(record);
          };
          const revision = () => JSON.stringify([child.state?.HEAD?.commit, this.tree.backendStatuses?.get(childKey)?.head_oid]);
          return this.linkDocuments.create(sideUri, child.rootUri.fsPath, side.filePath, currentLink, verify, revision);
        };
        const left = await resolve(plan.left);
        const right = await resolve(plan.right);
        if (!await checkHead()) throw new Error('The submodule changed; refresh and try again.');
        await this.vscode.commands.executeCommand('vscode.diff', left, right, `${path.basename(uri.fsPath)} (${record.group})`);
      } finally { session.close(); }
    } catch (error) {
      await this.vscode.window.showErrorMessage(`Gitpane submodule: ${error.message}`);
    }
  }

  async openPointer(repo, result, file) {
    if (file.is_submodule) throw new Error('Nested submodule changes cannot be opened here.');
    const uri = this.vscode.Uri.file(path.join(repo.rootUri.fsPath, file.path));
    const old = file.status === 'R' ? this.vscode.Uri.file(path.join(repo.rootUri.fsPath, file.old_path)) : uri;
    const empty = uri.with({ scheme: 'gitpane-empty', query: '' });
    const left = file.status === 'A' || !result.base_oid ? empty : this.git.toGitUri(old, result.base_oid);
    const right = file.status === 'D' ? empty : this.git.toGitUri(uri, result.head_oid);
    await this.vscode.commands.executeCommand('vscode.diff', left, right, `${path.basename(file.path)} (submodule)`);
  }

  dispose() { this.disposed = true; this.generation++; this.clearGraphWatch(); this.graphSubscription?.dispose(); }
}

module.exports = { SubmoduleProvider };
