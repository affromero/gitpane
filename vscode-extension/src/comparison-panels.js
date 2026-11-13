'use strict';

class ComparisonPanels {
  constructor(GraphProvider, tree, backend, git, extensionUri, theme, links) {
    Object.assign(this, { GraphProvider, tree, backend, git, extensionUri, theme, links });
    this.panels = new Set();
    this.preferences = undefined;
  }

  configureView(preferences) {
    this.preferences = preferences;
    for (const panel of this.panels) panel.configureView(preferences);
  }

  open(repo, guard, validate, persistent) {
    const graph = new this.GraphProvider(this.tree, this.backend, this.git, this.extensionUri, this.theme, this.links,
      { comparison: true });
    if (this.preferences) graph.configureView(this.preferences);
    try { graph.open(repo, guard, validate, persistent); }
    catch (error) { graph.dispose(); throw error; }
    this.panels.add(graph);
    graph.panel.onDidDispose(() => { this.panels.delete(graph); graph.dispose(); });
    return graph;
  }

  dispose() { for (const panel of this.panels) panel.dispose(); this.panels.clear(); }
}

module.exports = { ComparisonPanels };
