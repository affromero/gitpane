'use strict';

const path = require('node:path');

function text(value) {
  return value.replace(/[\x00-\x1f\x7f\u202a-\u202e\u2066-\u2069]/g, ' ')
    .replace(/\$\(/g, '＄(').trim().slice(0, 200) || 'Launcher';
}

function validString(value) {
  return typeof value === 'string' && value.length > 0 && !value.includes('\0');
}

function supportedExecutable(value) {
  const name = path.win32.basename(path.posix.basename(value)).toLowerCase();
  if (/\.(?:bat|cmd|ps1|sh)$/.test(name)) return false;
  const base = name.replace(/\.exe$/, '');
  return !/^(?:sh|bash|zsh|dash|ksh|fish|csh|tcsh|ash|cmd|powershell|pwsh|osascript|env|perl|ruby|node|deno|bun|php|lua|awk|sed|xargs|python(?:\d+(?:\.\d+)*)?)$/.test(base);
}

function terminalOptions(plan, repo, title, fingerprint) {
  if (!plan || plan.fingerprint !== fingerprint) throw new Error('Launcher configuration changed. Choose it again.');
  const options = { name: `Gitpane: ${text(title)}`, cwd: repo.rootUri };
  if (plan.kind === 'terminal') return options;
  if (plan.kind === 'argv') {
    if (!Array.isArray(plan.argv) || !plan.argv.length || !validString(plan.argv[0]) ||
        !plan.argv.every(arg => typeof arg === 'string' && !arg.includes('\0')) ||
        !supportedExecutable(plan.argv[0])) {
      throw new Error('Unsupported or invalid launcher command.');
    }
    options.shellPath = plan.argv[0];
    options.shellArgs = plan.argv.slice(1);
    return options;
  }
  if (plan.kind === 'shell') {
    const env = plan.env;
    if (process.platform === 'win32' || !validString(plan.script) || !env || typeof env !== 'object' ||
        Array.isArray(env) || Object.keys(env).sort().join(',') !== 'GITPANE_LAUNCH_BASE,GITPANE_LAUNCH_PATH' ||
        env.GITPANE_LAUNCH_BASE !== '' || !validString(env.GITPANE_LAUNCH_PATH)) {
      throw new Error('Unsupported or invalid shell launcher plan.');
    }
    options.shellPath = 'sh';
    options.shellArgs = ['-c', plan.script];
    options.env = { GITPANE_LAUNCH_PATH: env.GITPANE_LAUNCH_PATH, GITPANE_LAUNCH_BASE: env.GITPANE_LAUNCH_BASE };
    return options;
  }
  throw new Error('Unsupported launcher placement.');
}

class LauncherProvider {
  constructor(tree, backend, options = {}) {
    this.tree = tree;
    this.backend = backend;
    this.vscode = options.vscode || require('vscode');
    this.generation = 0;
    this.disposed = false;
  }

  current(repo, generation, guard = () => true) {
    if (!guard() || !repo?.rootUri || this.disposed || generation !== this.generation) return null;
    const current = this.tree.git.repositories.find(item => item.rootUri.toString() === repo.rootUri.toString());
    return current && this.tree.isVisible(current) ? current : null;
  }

  requireCurrent(repo, generation, guard) {
    const current = this.current(repo, generation, guard);
    if (!current) throw new Error('Repository or workspace changed; choose the launcher again.');
    return current;
  }

  async open(repo, guard = () => true) {
    const generation = ++this.generation;
    const token = repo && this.tree.repositoryToken(repo);
    const currentGuard = () => guard() && this.tree.isCurrentRepository(repo, token);
    let terminal;
    try {
      this.requireCurrent(repo, generation, currentGuard);
      const result = await this.backend.listLaunchers(repo.rootUri.fsPath);
      this.requireCurrent(repo, generation, currentGuard);
      if (!result || !Array.isArray(result.actions)) throw new Error('Gitpane returned an invalid launcher list.');
      const ids = new Set();
      const choices = result.actions.map(action => {
        if (!action || !validString(action.id) || !validString(action.label) || !['open', 'keybinding'].includes(action.kind) ||
            !validString(action.fingerprint) || ids.has(action.id)) throw new Error('Gitpane returned an invalid launcher list.');
        ids.add(action.id);
        return { label: text(action.label), id: action.id, fingerprint: action.fingerprint };
      });
      if (!choices.length) {
        await this.vscode.window.showInformationMessage('No configured Gitpane launchers for this repository.');
        return;
      }
      const selected = await this.vscode.window.showQuickPick(choices, { placeHolder: 'Choose a Gitpane launcher' });
      this.requireCurrent(repo, generation, currentGuard);
      if (!selected || !choices.includes(selected)) return;
      const plan = await this.backend.planLauncher(repo.rootUri.fsPath, selected.id, selected.fingerprint);
      let current = this.requireCurrent(repo, generation, currentGuard);
      const options = terminalOptions(plan, current, selected.label, selected.fingerprint);
      const validation = await this.backend.validateLauncher(repo.rootUri.fsPath, selected.id, selected.fingerprint);
      current = this.requireCurrent(repo, generation, currentGuard);
      if (!validation || validation.valid !== true) throw new Error('Launcher configuration changed. Choose it again.');
      options.cwd = current.rootUri;
      terminal = this.vscode.window.createTerminal(options);
      terminal.show();
    } catch (error) {
      terminal?.dispose();
      await this.vscode.window.showErrorMessage(`Gitpane launcher: ${error?.message || String(error)}`);
    }
  }

  dispose() {
    this.disposed = true;
    this.generation++;
  }
}

module.exports = { LauncherProvider };
