'use strict';

function label(value) {
  const text = value.replace(/[\x00-\x1f\x7f\u202a-\u202e\u2066-\u2069]/g, ' ')
    .replace(/\$\(/g, '＄(').trim();
  return text || 'Session';
}

class SessionProvider {
  constructor(tree, backend, options = {}) {
    this.tree = tree;
    this.backend = backend;
    this.vscode = options.vscode || require('vscode');
    this.generation = 0;
    this.disposed = false;
  }

  current(repo, generation) {
    if (!repo?.rootUri || this.disposed || generation !== this.generation) return false;
    const key = repo.rootUri.toString();
    const current = this.tree.git.repositories.find(item => item.rootUri.toString() === key);
    return !!current && this.tree.isVisible(current);
  }

  requireCurrent(repo, generation) {
    if (!this.current(repo, generation)) throw new Error('Repository or workspace changed; reopen sessions.');
  }

  async open(repo) {
    const generation = ++this.generation;
    try {
      this.requireCurrent(repo, generation);
      const backends = [{ label: 'tmux', kind: 'tmux' }, { label: 'herdr', kind: 'herdr' }];
      const selectedBackend = await this.vscode.window.showQuickPick(backends, { placeHolder: 'Choose a session type' });
      this.requireCurrent(repo, generation);
      if (!selectedBackend || !backends.includes(selectedBackend)) return;

      const result = await this.backend.listSessions(repo.rootUri.fsPath, selectedBackend.kind);
      this.requireCurrent(repo, generation);
      if (!result || !Array.isArray(result.sessions) ||
        result.sessions.some(session => typeof session !== 'string' || !session || session.includes('\0'))) {
        throw new Error('Gitpane returned an invalid session list.');
      }
      if (!result.sessions.length) {
        await this.vscode.window.showInformationMessage(`No ${selectedBackend.label} sessions found for this repository.`);
        return;
      }
      const choices = result.sessions.map(session => ({ label: label(session), session }));
      const selected = await this.vscode.window.showQuickPick(choices, { placeHolder: 'Choose a session to open' });
      this.requireCurrent(repo, generation);
      if (!selected || !choices.includes(selected)) return;
      await this.backend.gotoSession(repo.rootUri.fsPath, selectedBackend.kind, selected.session,
        () => this.current(repo, generation));
    } catch (error) {
      await this.vscode.window.showErrorMessage(`Gitpane sessions: ${error?.message || String(error)}`);
    }
  }

  dispose() {
    this.disposed = true;
    this.generation++;
  }
}

module.exports = { SessionProvider };
