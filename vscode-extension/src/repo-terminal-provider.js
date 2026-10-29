'use strict';

const path = require('node:path');

function terminalName(root) {
  const name = path.basename(root).replace(/[\x00-\x1f\x7f\u202a-\u202e\u2066-\u2069]/g, ' ')
    .replace(/\$\(/g, '＄(').trim().slice(0, 100);
  return `Gitpane: ${name || 'Repository'}`;
}

class RepoTerminalProvider {
  constructor(tree, options = {}) {
    this.tree = tree;
    this.vscode = options.vscode || require('vscode');
    this.disposed = false;
  }

  async open(repo, guard = () => true) {
    let terminal;
    try {
      if (!guard() || this.disposed || !repo?.rootUri) throw new Error('Repository is no longer in the workspace.');
      const token = this.tree.repositoryToken(repo);
      const key = repo.rootUri.toString();
      const current = this.tree.git.repositories.find(item => item.rootUri.toString() === key);
      if (!current || !this.tree.isCurrentRepository(current, token)) throw new Error('Repository is no longer in the workspace.');
      terminal = this.vscode.window.createTerminal({ name: terminalName(current.rootUri.fsPath), cwd: current.rootUri });
      terminal.show();
    } catch (error) {
      terminal?.dispose();
      await this.vscode.window.showErrorMessage(`Gitpane could not open the terminal: ${error.message}`);
    }
  }

  dispose() { this.disposed = true; }
}

module.exports = { RepoTerminalProvider };
