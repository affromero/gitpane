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

  current(repo, generation, guard = () => true) {
    if (!guard() || !repo?.rootUri || this.disposed || generation !== this.generation) return false;
    const key = repo.rootUri.toString();
    const current = this.tree.git.repositories.find(item => item.rootUri.toString() === key);
    return !!current && this.tree.isVisible(current);
  }

  requireCurrent(repo, generation, guard) {
    if (!this.current(repo, generation, guard)) throw new Error('Repository or workspace changed; reopen sessions.');
  }

  async open(repo, guard = () => true) {
    const generation = ++this.generation;
    const token = repo && this.tree.repositoryToken(repo);
    const currentGuard = () => guard() && this.tree.isCurrentRepository(repo, token);
    try {
      this.requireCurrent(repo, generation, currentGuard);
      const backends = [{ label: 'tmux', kind: 'tmux' }, { label: 'herdr', kind: 'herdr' }];
      const selectedBackend = await this.vscode.window.showQuickPick(backends, { placeHolder: 'Choose a session type' });
      this.requireCurrent(repo, generation, currentGuard);
      if (!selectedBackend || !backends.includes(selectedBackend)) return;

      const result = await this.backend.listSessions(repo.rootUri.fsPath, selectedBackend.kind);
      this.requireCurrent(repo, generation, currentGuard);
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
      this.requireCurrent(repo, generation, currentGuard);
      if (!selected || !choices.includes(selected)) return;
      await this.backend.gotoSession(repo.rootUri.fsPath, selectedBackend.kind, selected.session,
        () => this.current(repo, generation, currentGuard));
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
