'use strict';

const path = require('node:path');

function validFilePath(value) {
  return typeof value === 'string' && !!value && !path.isAbsolute(value) &&
    !value.split(/[\\/]/).some(part => !part || part === '.' || part === '..');
}

class BranchReviewProvider {
  constructor(tree, backend, git, options = {}) {
    this.tree = tree;
    this.backend = backend;
    this.git = git;
    this.vscode = options.vscode || require('vscode');
    this.generation = 0;
    this.disposed = false;
  }

  current(repo, generation, head) {
    const current = this.tree.git.repositories.find(item => item.rootUri.toString() === repo.rootUri.toString());
    return !this.disposed && this.generation === generation && !!current && this.tree.isVisible(current) &&
      (!head || current.state?.HEAD?.commit === head);
  }

  async open(repo) {
    const generation = ++this.generation;
    if (!repo || !this.current(repo, generation)) {
      await this.vscode.window.showErrorMessage('Gitpane branch review: repository is no longer in the workspace.');
      return;
    }
    try {
      const result = await this.backend.branchReview(repo.rootUri.fsPath);
      if (!this.current(repo, generation, result?.head_oid)) throw new Error('Repository or HEAD changed; refresh and try again.');
      if (!result || typeof result.base_ref !== 'string' || !/^[a-f0-9]{40,64}$/.test(result.base_oid) ||
        !/^[a-f0-9]{40,64}$/.test(result.head_oid) || !Array.isArray(result.files)) {
        throw new Error('Gitpane returned an invalid branch review.');
      }
      const choices = result.files.map(file => {
        if (!file || !validFilePath(file.path) || !['A', 'D', 'M', 'R', '?'].includes(file.status) ||
          (file.status === 'R' && !validFilePath(file.old_path)) || typeof file.is_submodule !== 'boolean') {
          throw new Error('Gitpane returned an invalid branch review file.');
        }
        return { label: file.path, description: file.status, file };
      });
      if (!choices.length) {
        await this.vscode.window.showInformationMessage(`No committed changes since ${result.base_ref}.`);
        return;
      }
      const selected = await this.vscode.window.showQuickPick(choices, {
        placeHolder: `Review changes since ${result.base_ref}`,
        matchOnDescription: true
      });
      if (!selected || !choices.includes(selected)) return;
      if (!this.current(repo, generation, result.head_oid)) throw new Error('Repository or HEAD changed; refresh and try again.');
      const file = selected.file;
      if (file.is_submodule) throw new Error('Submodule changes cannot be opened as a file diff yet.');
      const uri = this.vscode.Uri.file(path.join(repo.rootUri.fsPath, file.path));
      const oldUri = file.status === 'R' ? this.vscode.Uri.file(path.join(repo.rootUri.fsPath, file.old_path)) : uri;
      const empty = uri.with({ scheme: 'gitpane-empty', query: '' });
      const left = file.status === 'A' ? empty : this.git.toGitUri(oldUri, result.base_oid);
      const right = file.status === 'D' ? empty : this.git.toGitUri(uri, result.head_oid);
      await this.vscode.commands.executeCommand('vscode.diff', left, right, `${path.basename(file.path)} (${result.base_ref}...HEAD)`);
    } catch (error) {
      await this.vscode.window.showErrorMessage(`Gitpane branch review: ${error.message}`);
    }
  }

  dispose() {
    this.disposed = true;
    this.generation++;
  }
}

module.exports = { BranchReviewProvider };
