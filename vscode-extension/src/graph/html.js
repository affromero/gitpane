'use strict';

const vscode = require('vscode');

function getHtml(webview, extensionUri) {
  const resource = name => webview.asWebviewUri(
    vscode.Uri.joinPath(extensionUri, 'src', 'graph', name)
  );
  return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src ${webview.cspSource}; script-src ${webview.cspSource};">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <link rel="stylesheet" href="${resource('styles.css')}">
  <title>Gitpane Graph</title>
</head>
<body>
  <header class="toolbar">
    <label class="search-label" for="search">Search commits</label>
    <input id="search" type="search" placeholder="Search commits" autocomplete="off" spellcheck="false">
    <button id="search-prev" type="button" title="Previous match" aria-label="Previous commit match">↑</button>
    <button id="search-next" type="button" title="Next match" aria-label="Next commit match">↓</button>
    <label class="first-parent"><input id="first-parent" type="checkbox"> First parent</label>
    <button id="filters" type="button" aria-label="Filter commit graph">Filters</button>
    <button id="collapse" type="button" disabled>Collapse branch</button>
    <button id="expand-all" type="button" disabled>Expand all</button>
    <button id="refresh" type="button" title="Refresh commit graph" aria-label="Refresh commit graph">↻</button>
  </header>
  <div id="search-status" class="search-status" role="status" aria-live="polite"></div>
  <div id="graph-status" class="message" role="status" aria-live="polite"></div>
  <div id="graph" class="graph" role="listbox" aria-label="Commit graph" tabindex="0"></div>
  <section class="details" aria-label="Commit files">
    <h2 id="files-heading">Commit files</h2>
    <label class="search-label" for="file-search">Search commit files</label>
    <input id="file-search" type="search" placeholder="Search files" autocomplete="off" spellcheck="false" disabled>
    <div id="files-status" class="message" role="status" aria-live="polite">Select a commit to see its files.</div>
    <ul id="files" class="files" aria-labelledby="files-heading"></ul>
  </section>
  <script src="${resource('view.js')}"></script>
</body>
</html>`;
}

module.exports = { getHtml };
