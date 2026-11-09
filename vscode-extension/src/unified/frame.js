'use strict';

(() => {
  const surface = document.currentScript.dataset.surface;
  const api = window.parent.GitpaneFrameApi(surface);
  window.acquireVsCodeApi = () => api;
})();
