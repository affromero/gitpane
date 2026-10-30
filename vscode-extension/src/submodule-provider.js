'use strict';

const { statusRevision } = require('./link-documents');

const fs = require('node:fs');
const path = require('node:path');
const { isSubmodule, richComparison } = require('./model');
const { relativeFile: safeRelative, scopedFile, validRecord, validatePage } = require('./change-pager');
const { isPointerComparison, openPointerDiff } = require('./submodule-pointer-diff');
const { openGitlinkDiff } = require('./gitlink-diff');
const { createLiveGitSide } = require('./git-side-documents');

class SubmoduleProvider {
  constructor(tree, backend, git, options = {}) {
    this.tree = tree;
    this.backend = backend;
    this.git = git;
    this.vscode = options.vscode || require('vscode');
    this.resolveChange = options.resolveChange;
    this.openGraph = options.openGraph;
    this.linkDocuments = options.linkDocuments;
    this.pins = options.pins;
    this.generation = 0;
    this.disposed = false;
    this.graphWatch = undefined;
    this.openingGuard = undefined;
    this.graphSubscription = tree.onDidChangeTreeData?.(() => this.scheduleGraphCheck());
  }

  current(node, generation, token) {
    if (this.disposed || generation !== this.generation || this.openingGuard?.() === false || !this.tree.isCurrentRepository(node?.repo, token)) return undefined;
    const now = this.resolveChange(node);
    if (!now || !this.tree.isVisible(now.repo) || now.group !== node.group ||
      now.change.status !== node.change.status || now.change.uri.toString() !== node.change.uri.toString()) return undefined;
    return now;
  }

  childCurrent(node, generation, childPath, parentToken, childRepo, childToken, persistent = false) {
    if (this.disposed || (!persistent && (generation !== this.generation || this.openingGuard?.() === false)) ||
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
      if (childRepo && fs.realpathSync.native(childRepo.rootUri.fsPath) !== resolved) return false;
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

  async openGuardedGraph(child, node, current, checkAll, persistent) {
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
    try { return await this.openGraph(child, () => watch.valid && current(), validate, persistent); }
    catch (error) { if (this.graphWatch === watch) this.clearGraphWatch(); throw error; }
  }

  async open(selected, pinDirect = false, guard = () => true) {
    if (!guard()) return;
    this.clearGraphWatch();
    const generation = ++this.generation;
    this.openingGuard = guard;
    try {
      await this.openLevel(selected, generation, [], pinDirect);
    } catch (error) {
      if (guard()) await this.vscode.window.showErrorMessage(`Gitpane submodule: ${error.message}`);
    } finally { if (this.generation === generation) this.openingGuard = undefined; }
  }

  async openLevel(selected, generation, ancestors, pinDirect = false) {
      const parentToken = selected?.repo && this.tree.repositoryToken(selected.repo);
      const currentNode = ancestors.length ? selected : this.current(selected, generation, parentToken);
      if (!currentNode) throw new Error('The submodule is no longer in the workspace.');
      const node = currentNode.record ? { ...currentNode, record: structuredClone(currentNode.record) } : currentNode;
      if (ancestors.length >= 32) throw new Error('The submodule nesting limit was reached.');
      if (!validRecord(node.record)) throw new Error('The selected submodule change is invalid.');
      const checkFrames = async (frames, persistent = false) => {
        const current = () => frames.every(frame => persistent ? frame.currentLink() : frame.current());
        if (!current()) return false;
        if (!frames.length) return true;
        const result = await this.backend.validateSubmoduleChain(frames.map(frame => frame.link));
        if (!result || typeof result.valid !== 'boolean') throw new Error('Gitpane returned an invalid submodule validation result.');
        return result.valid && current();
      };
      const relative = node.record.path;
      const selectedPath = path.join(node.repo.rootUri.fsPath, ...relative.split('/'));
      if (!safeRelative(relative) || path.relative(selectedPath, node.change.uri.fsPath) || !(node.record?.is_submodule ||
        isSubmodule(node.repo.rootUri.fsPath, node.change.uri.fsPath, node.repo.state.submodules))) {
        throw new Error('The selected change is not a current submodule.');
      }
      const group = node.record?.group || node.group;
      const originalPath = node.record?.kind === 'index_renamed' ? node.record.original_path : undefined;
      if (originalPath !== undefined && !safeRelative(originalPath)) throw new Error('The submodule has an invalid original path.');
      const result = await this.backend.inspectSubmodule(node.repo.rootUri.fsPath, relative, group, originalPath);
      const validOid = value => value === null || (typeof value === 'string' && /^[a-f0-9]{40,64}$/.test(value));
      if (result && (!Object.hasOwn(result, 'parent_identity') || !Object.hasOwn(result, 'child_identity'))) {
        throw new Error('The configured Gitpane backend does not support submodule identity checks. Update the backend and try again.');
      }
      if (!result || typeof result.repo !== 'string' || !path.isAbsolute(result.repo) ||
        !validOid(result.base_oid) || !validOid(result.head_oid) ||
        typeof result.child_head_oid !== 'string' || !/^[a-f0-9]{40,64}$/.test(result.child_head_oid) ||
        typeof result.parent_identity !== 'string' || !/^[a-f0-9]{64}$/.test(result.parent_identity) ||
        typeof result.child_identity !== 'string' || !/^[a-f0-9]{64}$/.test(result.child_identity) ||
        !Array.isArray(result.pointer_files)) {
        throw new Error('Gitpane returned invalid submodule details.');
      }
      if (!this.childCurrent(node, generation, result.repo, parentToken)) throw new Error('The submodule is no longer in the workspace.');
      const childUri = this.vscode.Uri.file(result.repo);
      const revision = this.tree.repositoryRevision(childUri);
      const matchesChild = repo => this.childCurrent(node, generation, result.repo, parentToken, repo, this.tree.repositoryToken(repo));
      const existingChild = this.git.repositories.find(matchesChild);
      const existingToken = existingChild && this.tree.repositoryToken(existingChild);
      const link = { repo: node.repo.rootUri.fsPath, path: relative, group, original_path: node.record.original_path,
        record: node.record, child_repo: result.repo, base_oid: result.base_oid, head_oid: result.head_oid,
        child_head_oid: result.child_head_oid, parent_identity: result.parent_identity, child_identity: result.child_identity };
      const preFrame = { link, current: () => this.childCurrent(node, generation, result.repo, parentToken),
        currentLink: () => this.childCurrent(node, generation, result.repo, parentToken, undefined, undefined, true) };
      if (!await checkFrames([...ancestors, preFrame])) throw new Error('The submodule changed; refresh and try again.');
      const opened = await this.git.openRepository(childUri);
      const expectedRevision = revision + (existingToken === undefined ? 1 : 0);
      const child = this.git.repositories.find(matchesChild);
      const childToken = child && this.tree.repositoryToken(child);
      if (!opened || !matchesChild(opened) || !child ||
          this.tree.repositoryRevision(childUri) !== expectedRevision ||
          (existingToken !== undefined && existingToken !== childToken) ||
          !this.childCurrent(node, generation, result.repo, parentToken, child, childToken)) {
        throw new Error('The submodule is unavailable or no longer in the workspace.');
      }
      const fixedChoices = [{ label: 'Open Commit Graph', kind: 'graph' }];
      if (this.pins) fixedChoices.push({ label: 'Pin Submodule in Gitpane', kind: 'pin' });
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
      const contextKey = ancestors[0]?.contextKey || JSON.stringify(node.parentWorktree || null);
      const frame = { link, contextKey, current: () => ownCurrent(false), currentLink: () => ownCurrent(true) };
      const checkHead = (persistent = false) => checkFrames([...ancestors, frame], persistent);
      const pointerOptions = { parent: node.repo.rootUri.fsPath, relative, group, originalPath,
        childToken, currentChild, currentLink, checkHead,
        ancestorKey: JSON.stringify([...ancestors, frame].map(item => item.link)), contextKey };
      const pin = async () => {
        if (!this.pins) throw new Error('Submodule pins are unavailable.');
        const links = [...ancestors, frame].map(item => item.link);
        await this.pins.pin(links[0].repo, links, currentChild, checkHead);
      };
      if (!await checkHead()) throw new Error('The submodule HEAD changed; refresh and try again.');
      const initialChoices = [...fixedChoices, { label: 'Browse working changes', kind: 'browse' }];
      const initial = pinDirect ? initialChoices.find(choice => choice.kind === 'pin') :
        await this.vscode.window.showQuickPick(initialChoices, { placeHolder: `Inspect ${relative}`, matchOnDescription: true });
      if (!initial || !initialChoices.includes(initial)) return;
      if (!await checkHead()) throw new Error('The submodule changed; refresh and try again.');
      if (initial.kind === 'pin') return await pin();
      const graphContext = { current: currentLink, verifyAncestors: () => checkHead(true),
        ancestorKey: pointerOptions.ancestorKey, contextKey };
      if (initial.kind === 'graph') return await this.openGuardedGraph(child, node, currentChild, checkHead, graphContext);
      if (initial.kind === 'pointer') {
        return await this.openPointer(child, result, initial.file, pointerOptions);
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
        if (chosen.kind === 'graph') return await this.openGuardedGraph(child, node, currentChild, checkHead, graphContext);
        if (chosen.kind === 'pin') {
          session.close();
          sessionClosed = true;
          return await pin();
        }
        if (chosen.kind === 'pointer') {
          return await this.openPointer(child, result, chosen.file, pointerOptions);
        }
        const record = structuredClone(chosen.record);
        if (record.kind === 'untracked_directory') throw new Error('Open the nested repository to inspect its changes.');
        const checked = await this.backend.verifyChanges(child.rootUri.fsPath, record);
        if (!await checkHead() || !validRecord(checked?.change) || JSON.stringify(checked.change) !== JSON.stringify(record)) {
          throw new Error('The submodule change is no longer available.');
        }
        // This URI identifies the Git record; it is not read from the filesystem.
        // A changed file or link is checked separately before it is opened.
        const selectedPath = path.join(child.rootUri.fsPath, ...record.path.split('/'));
        const uri = this.vscode.Uri.file(selectedPath);
        if (record.is_submodule || isSubmodule(child.rootUri.fsPath, uri.fsPath, child.state.submodules)) {
          session.close();
          sessionClosed = true;
          if (isPointerComparison(record)) {
            if (!this.linkDocuments) throw new Error('Submodule pointer diffs are unavailable.');
            const nested = { repo: child, repoToken: childToken, group: record.group, record, change: { uri }, fileId: record.path };
            const pointerProvider = {
              currentChange: () => currentChild() ? nested : undefined,
              waitForCurrentChange: async () => currentChild() ? nested : undefined,
              withCurrentChange: async (_node, dispatch) => {
                if (!currentChild()) return false;
                const result = dispatch(nested);
                if (result === false) return false;
                await result;
                return true;
              },
              revalidateChange: async () => {
                if (!await checkHead() || !currentChild()) return false;
                const latest = await this.backend.verifyChanges(child.rootUri.fsPath, record);
                return currentChild() && validRecord(latest?.change) && JSON.stringify(latest.change) === JSON.stringify(record);
              }
            };
            return await openPointerDiff(nested, pointerProvider, this.tree, this.backend, this.linkDocuments, this.vscode, {
              repoIdentity: result.child_identity, current: currentLink, verifyAncestors: () => checkHead(true),
              ancestorKey: JSON.stringify([...ancestors, frame].map(item => item.link)), contextKey
            });
          }
          if (record.group === 'working' && record.kind === 'working_renamed') {
            throw new Error('Working submodule renames cannot be compared yet.');
          }
          const nested = { repo: child, group: record.group, record, change: { uri } };
          return await this.openLevel(nested, generation, [...ancestors, frame]);
        }
        const plan = richComparison(record);
        const empty = uri.with({ scheme: 'gitpane-empty', query: '' });
        const resolve = async side => {
          if (side.empty) return empty;
          if (side.gitPath) return createLiveGitSide({ vscode: this.vscode, links: this.linkDocuments,
            backend: this.backend, repo: child, repoIdentity: result.child_identity, repoToken: childToken,
            side, current: currentLink, revision: () => this.tree.contentRevision?.(child), verifyAncestors: () => checkHead(true),
            ancestorKey: JSON.stringify([...ancestors, frame].map(item => item.link)), contextKey });
          const target = scopedFile(child.rootUri.fsPath, side.filePath);
          const sideUri = this.vscode.Uri.file(target);
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

  async openPointer(repo, result, selected, options) {
    const file = structuredClone(selected);
    if (file.is_submodule) {
      const verify = async () => {
        if (!options.currentChild() || !await options.checkHead()) return false;
        const latest = await this.backend.inspectSubmodule(options.parent, options.relative, options.group, options.originalPath);
        if (!options.currentChild() || !await options.checkHead() || !latest || latest.repo !== result.repo ||
            latest.parent_identity !== result.parent_identity || latest.child_identity !== result.child_identity ||
            latest.base_oid !== result.base_oid || latest.head_oid !== result.head_oid ||
            latest.child_head_oid !== result.child_head_oid || !Array.isArray(latest.pointer_files)) return false;
        return latest.pointer_files.some(item => item.path === file.path && item.status === file.status &&
          item.old_path === file.old_path && item.is_submodule === true &&
          JSON.stringify(item.gitlink) === JSON.stringify(file.gitlink));
      };
      return await openGitlinkDiff({ vscode: this.vscode, links: this.linkDocuments, repo,
        repoIdentity: result.child_identity, repoToken: options.childToken, file: { ...file, oldPath: file.old_path },
        oldRef: result.base_oid, newRef: result.head_oid, current: options.currentLink, verify,
        readHistorical: (commit, relative, oid, mode) => this.backend.submoduleHistoricalContent(repo.rootUri.fsPath,
          result.child_identity, commit, relative, oid, mode),
        verifyAncestors: () => options.checkHead(true), ancestorKey: options.ancestorKey, contextKey: options.contextKey,
        title: `${path.basename(file.path)} (submodule)` });
    }
    const uri = this.vscode.Uri.file(path.join(repo.rootUri.fsPath, file.path));
    const old = file.status === 'R' ? this.vscode.Uri.file(path.join(repo.rootUri.fsPath, file.old_path)) : uri;
    const empty = uri.with({ scheme: 'gitpane-empty', query: '' });
    const makeSide = (target, ref) => createLiveGitSide({ vscode: this.vscode, links: this.linkDocuments,
      backend: this.backend, repo, repoIdentity: result.child_identity, repoToken: options.childToken,
      side: { gitPath: path.relative(repo.rootUri.fsPath, target.fsPath).split(path.sep).join('/'), ref },
      current: options.currentLink, revision: () => this.tree.contentRevision?.(repo),
      verifyAncestors: () => options.checkHead(true), ancestorKey: options.ancestorKey,
      contextKey: options.contextKey });
    const left = file.status === 'A' || !result.base_oid ? empty : await makeSide(old, result.base_oid);
    const right = file.status === 'D' || !result.head_oid ? empty : await makeSide(uri, result.head_oid);
    if (!options.currentChild() || !await options.checkHead()) throw new Error('The submodule changed; refresh and try again.');
    await this.vscode.commands.executeCommand('vscode.diff', left, right, `${path.basename(file.path)} (submodule)`);
  }

  dispose() { this.disposed = true; this.generation++; this.clearGraphWatch(); this.graphSubscription?.dispose(); }
}

module.exports = { SubmoduleProvider };
