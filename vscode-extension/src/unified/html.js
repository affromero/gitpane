'use strict';

const vscode = require('vscode');
const fs = require('node:fs');
const path = require('node:path');
const { randomBytes } = require('node:crypto');
const { getHtml: treeHtml } = require('../webview/html');
const { getHtml: graphHtml } = require('../graph/html');

function escapeAttribute(value) {
  return value.replaceAll('&', '&amp;').replaceAll('"', '&quot;').replaceAll('<', '&lt;').replaceAll('>', '&gt;');
}

function getHtml(webview, extensionUri) {
  const resource = name => webview.asWebviewUri(vscode.Uri.joinPath(extensionUri, 'src', 'unified', name));
  const base = typeof extensionUri === 'string' ? extensionUri : extensionUri.fsPath;
  const nonce = randomBytes(18).toString('base64');
  const asset = name => fs.readFileSync(path.join(base, 'src', name), 'utf8');
  const script = name => `<script nonce="${nonce}">${asset(name).replace(/<\/script/gi, '<\\/script')}</script>`;
  const style = name => `<style nonce="${nonce}">${asset(name).replace(/<\/style/gi, '<\\/style')}</style>`;
  const frame = (surface, html) => {
    const kind = surface === 'graph' ? 'graph' : 'webview';
    const uri = name => String(webview.asWebviewUri(vscode.Uri.joinPath(extensionUri, 'src', name)));
    const bootstrap = script('unified/frame.js').replace('<script ', `<script data-surface="${surface}" `);
    const content = html.replace(`style-src ${webview.cspSource}; script-src ${webview.cspSource};`,
      `style-src 'nonce-${nonce}'; script-src 'nonce-${nonce}';`)
      .replace(`<link rel="stylesheet" href="${uri(`${kind}/styles.css`)}">`, style(`${kind}/styles.css`))
      .replace(`<script src="${uri('theme-webview.js')}"></script>`, `${bootstrap}\n  ${script('theme-webview.js')}`)
      .replace(`<script src="${uri('webview/file-menu.js')}"></script>`, script('webview/file-menu.js'))
      .replace(`<script src="${uri(`${kind}/view.js`)}"></script>`, script(`${kind}/view.js`));
    return escapeAttribute(content);
  };
  const repositories = frame('repositories', treeHtml(webview, extensionUri, 'repositories'));
  const changes = frame('changes', treeHtml(webview, extensionUri, 'changes'));
  const graph = frame('graph', graphHtml(webview, extensionUri, true));
  return `<!DOCTYPE html>
<html lang="en"><head><meta charset="UTF-8">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; frame-src 'self' ${webview.cspSource}; style-src ${webview.cspSource} 'nonce-${nonce}'; script-src ${webview.cspSource} 'nonce-${nonce}';">
<meta name="viewport" content="width=device-width, initial-scale=1">
<link rel="stylesheet" href="${resource('styles.css')}"><title>Gitpane</title></head>
<body><main id="panes" aria-label="Gitpane workspace">
  <section class="pane" id="repositories-pane" aria-labelledby="repositories-title"><h2 id="repositories-title">Repositories</h2><iframe title="Repositories" data-surface="repositories" sandbox="allow-scripts allow-same-origin" data-document="${repositories}"></iframe></section>
  <div class="splitter" role="separator" tabindex="0" aria-label="Resize repositories and changes" aria-orientation="horizontal" aria-controls="repositories-pane changes-pane"></div>
  <section class="pane" id="changes-pane" aria-labelledby="changes-title"><h2 id="changes-title">Changes</h2><iframe title="Changes" data-surface="changes" sandbox="allow-scripts allow-same-origin" data-document="${changes}"></iframe></section>
  <div class="splitter" role="separator" tabindex="0" aria-label="Resize changes and Git graph" aria-orientation="horizontal" aria-controls="changes-pane graph-pane"></div>
  <section class="pane" id="graph-pane" aria-labelledby="graph-title"><h2 id="graph-title">Git Graph</h2><iframe title="Git Graph" data-surface="graph" sandbox="allow-scripts allow-same-origin" data-document="${graph}"></iframe></section>
</main><script src="${resource('view.js')}"></script></body></html>`;
}

module.exports = { getHtml };
