'use strict';

const DEFAULT_REFRESH_INTERVAL = 5000;
const KEYS = ['sortOrder', 'worktrees.defaultVisibility', 'refreshInterval', 'showSessionLiveness',
  'graph.firstParent', 'graph.labelMaxLength'];

function readPreferences(configuration, rememberedOrder) {
  const read = (key, fallback, valid) => {
    const value = configuration.get(key, fallback);
    if (!valid(value)) throw new Error(`Invalid value for gitpane.${key}.`);
    return value;
  };
  const orders = ['workspace', 'alphabetical', 'reverse', 'dirty'];
  const order = read('sortOrder', 'remember', value => value === 'remember' || orders.includes(value));
  const visibility = read('worktrees.defaultVisibility', 'native', value => ['native', 'expanded', 'collapsed'].includes(value));
  const seconds = read('refreshInterval', 5, value => Number.isSafeInteger(value) && value >= 1 && value <= 300);
  const labelLength = read('graph.labelMaxLength', -1, value => Number.isSafeInteger(value) && value >= -1 && value <= 120);
  return {
    sortOrder: order === 'remember' ? (orders.includes(rememberedOrder) ? rememberedOrder : 'workspace') : order,
    refreshInterval: seconds * 1000,
    worktreeExpansion: visibility === 'native' ? undefined : visibility === 'expanded',
    showSessionLiveness: read('showSessionLiveness', true, value => typeof value === 'boolean'),
    graphFirstParent: read('graph.firstParent', false, value => typeof value === 'boolean'),
    graphLabelMaxLength: labelLength === -1 ? undefined : labelLength
  };
}

class ViewSettings {
  constructor(vscode, context, tree, provider, graph) {
    this.vscode = vscode;
    this.context = context;
    this.tree = tree;
    this.provider = provider;
    this.graphs = Array.isArray(graph) ? graph : [graph];
    this.subscription = vscode.workspace.onDidChangeConfiguration(event => {
      if (KEYS.some(key => event.affectsConfiguration(`gitpane.${key}`))) this.apply();
    });
    this.command = vscode.commands.registerCommand('gitpane.openSettings', () =>
      vscode.commands.executeCommand('workbench.action.openSettings', `@ext:${context.extension.id}`));
    this.apply();
  }

  apply() {
    try {
      const configuration = this.vscode.workspace.getConfiguration('gitpane');
      const preferences = readPreferences(configuration, this.context.workspaceState?.get('gitpane.sortOrder'));
      this.tree.configureView(preferences);
      this.provider.configureView(preferences);
      for (const graph of this.graphs) graph.configureView(preferences);
      this.tree.setSortOrder(preferences.sortOrder);
    } catch (error) { this.vscode.window.showErrorMessage(`Gitpane settings: ${error.message}`); }
  }

  async chooseSortOrder(order) {
    if (!['workspace', 'alphabetical', 'reverse', 'dirty'].includes(order)) throw new Error('Invalid repository sort order.');
    const configuration = this.vscode.workspace.getConfiguration('gitpane');
    if (configuration.get('sortOrder', 'remember') !== 'remember') {
      const workspace = this.vscode.workspace.workspaceFolders?.length || this.vscode.workspace.workspaceFile;
      await configuration.update('sortOrder', order,
        workspace ? this.vscode.ConfigurationTarget.Workspace : this.vscode.ConfigurationTarget.Global);
    }
    await this.context.workspaceState?.update('gitpane.sortOrder', order);
    this.tree.setSortOrder(order);
  }

  dispose() { this.subscription.dispose(); this.command.dispose(); }
}

module.exports = { DEFAULT_REFRESH_INTERVAL, ViewSettings, readPreferences };
