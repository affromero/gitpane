'use strict';

const vscode = require('vscode');

function getHtml(webview, extensionUri) {
  const resource = name => webview.asWebviewUri(
    vscode.Uri.joinPath(extensionUri, 'src', 'webview', name)
  );
  const stylesheet = resource('styles.css');
  const script = resource('view.js');
  const theme = webview.asWebviewUri(vscode.Uri.joinPath(extensionUri, 'src', 'theme-webview.js'));
  return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src ${webview.cspSource}; script-src ${webview.cspSource};">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <link rel="stylesheet" href="${stylesheet}">
  <title>Gitpane</title>
</head>
<body>
  <div id="messages" aria-live="polite"></div>
  <div class="search-toolbar">
    <button id="start-search" type="button" aria-label="Search changed files" title="Search changed files">⌕</button>
    <div id="search-box" hidden>
      <label class="search-label" for="file-search">Search changed files</label>
      <input id="file-search" type="search" placeholder="Search changed files" autocomplete="off" spellcheck="false">
      <button id="search-prev" type="button" aria-label="Previous file match" disabled>↑</button>
      <button id="search-next" type="button" aria-label="Next file match" disabled>↓</button>
      <div id="search-status" role="status" aria-live="polite"></div>
    </div>
  </div>
  <div id="repositories" role="tree" aria-label="Gitpane repositories"></div>
  <script src="${theme}"></script>
  <script src="${script}"></script>
</body>
</html>`;
}

module.exports = { getHtml };
