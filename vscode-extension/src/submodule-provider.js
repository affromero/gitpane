'use strict';

const fs = require('node:fs');
const path = require('node:path');
const { comparison, groups, isSubmodule } = require('./model');

function safeRelative(value) {
  return typeof value === 'string' && !!value && !path.isAbsolute(value) &&
    !value.split(/[\\/]/).some(part => !part || part === '.' || part === '..');
}

class SubmoduleProvider {
  constructor(tree, backend, git, options = {}) {
    this.tree = tree;
    this.backend = backend;
    this.git = git;
    this.vscode = options.vscode || require('vscode');
    this.resolveChange = options.resolveChange;
    this.openGraph = options.openGraph;
    this.generation = 0;
    this.disposed = false;
  }

  current(node, generation, token) {
    if (this.disposed || generation !== this.generation || !this.tree.isCurrentRepository(node?.repo, token)) return undefined;
    const now = this.resolveChange(node);
    if (!now || !this.tree.isVisible(now.repo) || now.group !== node.group ||
      now.change.status !== node.change.status || now.change.uri.toString() !== node.change.uri.toString()) return undefined;
    return now;
  }

  childCurrent(node, generation, childPath, parentToken, childRepo, childToken) {
    const current = this.current(node, generation, parentToken);
    if (!current || !this.tree.isPathInWorkspace(childPath)) return false;
    try {
      const original = fs.realpathSync.native(node.change.uri.fsPath);
      const resolved = fs.realpathSync.native(childPath);
      if (original !== resolved) return false;
      return !childRepo || this.tree.isCurrentRepository(childRepo, childToken);
    } catch { return false; }
  }

  async open(selected) {
    const generation = ++this.generation;
    const parentToken = selected?.repo && this.tree.repositoryToken(selected.repo);
    try {
      const node = this.current(selected, generation, parentToken);
      if (!node) throw new Error('The submodule is no longer in the workspace.');
      const relative = path.relative(node.repo.rootUri.fsPath, node.change.uri.fsPath);
      if (!safeRelative(relative) || !isSubmodule(node.repo.rootUri.fsPath, node.change.uri.fsPath, node.repo.state.submodules)) {
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
      await child.status();
      if (!this.childCurrent(node, generation, result.repo, parentToken, child, childToken)) throw new Error('The submodule is no longer in the workspace.');
      if (child.state?.HEAD?.commit !== result.head_oid) throw new Error('The submodule HEAD changed; refresh and try again.');
      const choices = [{ label: 'Open Commit Graph', kind: 'graph' }];
      for (const file of result.pointer_files) {
        if (!file || !safeRelative(file.path) || !['A', 'D', 'M', 'R', '?'].includes(file.status) ||
          (file.status === 'R' && !safeRelative(file.old_path)) || typeof file.is_submodule !== 'boolean') {
          throw new Error('Gitpane returned an invalid submodule change.');
        }
        choices.push({ label: file.path, description: `Committed ${file.status}`, kind: 'pointer', file });
      }
      for (const group of groups(child.state)) {
        for (const change of group.changes) {
          const relative = path.relative(child.rootUri.fsPath, change.uri.fsPath);
          if (!safeRelative(relative)) throw new Error('Gitpane returned an invalid submodule change.');
          choices.push({ label: relative, description: group.label, kind: 'change', change, group: group.id });
        }
      }
      const chosen = await this.vscode.window.showQuickPick(choices, { placeHolder: `Inspect ${relative}`, matchOnDescription: true });
      if (!chosen || !choices.includes(chosen)) return;
      const currentChild = () => this.childCurrent(node, generation, result.repo, parentToken, child, childToken);
      if (!currentChild() || child.state?.HEAD?.commit !== result.head_oid) {
        throw new Error('The submodule changed; refresh and try again.');
      }
      if (chosen.kind === 'graph') return await this.openGraph(child, currentChild);
      if (chosen.kind === 'pointer') return await this.openPointer(child, result, chosen.file);
      const currentGroup = groups(child.state).find(group => group.id === chosen.group);
      const currentChange = currentGroup?.changes.find(change => change.uri.toString() === chosen.change.uri.toString() && change.status === chosen.change.status);
      if (!currentChange) throw new Error('The submodule change is no longer available.');
      if (isSubmodule(child.rootUri.fsPath, currentChange.uri.fsPath, child.state.submodules)) {
        throw new Error('Nested submodule changes cannot be opened here.');
      }
      const plan = comparison(chosen.group, currentChange);
      if (plan.open) return await this.vscode.commands.executeCommand('vscode.open', plan.open);
      const empty = currentChange.uri.with({ scheme: 'gitpane-empty', query: '' });
      const resolve = side => side.empty ? empty : side.file || this.git.toGitUri(side.git, side.ref);
      await this.vscode.commands.executeCommand('vscode.diff', resolve(plan.left), resolve(plan.right), `${path.basename(currentChange.uri.fsPath)} (${chosen.group})`);
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

  dispose() { this.disposed = true; this.generation++; }
}

module.exports = { SubmoduleProvider };
