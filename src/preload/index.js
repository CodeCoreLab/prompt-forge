const { contextBridge, ipcRenderer } = require('electron');

contextBridge.exposeInMainWorld('promptForge', {
  enhancePrompt: (text, mode) => ipcRenderer.invoke('enhance', text, mode),
  cancelEnhance: () => ipcRenderer.invoke('cancelEnhance'),
  getModes: () => ipcRenderer.invoke('getModes'),
  getConfig: () => ipcRenderer.invoke('getConfig'),
  saveConfig: (config) => ipcRenderer.invoke('saveConfig', config),
  listModels: () => ipcRenderer.invoke('listModels'),
  addModel: (model) => ipcRenderer.invoke('addModel', model),
  removeModel: (id) => ipcRenderer.invoke('removeModel', id),
  getHistory: () => ipcRenderer.invoke('getHistory'),
  copyToClipboard: (text) => ipcRenderer.invoke('copyToClipboard', text),
  testConnection: (profile) => ipcRenderer.invoke('testConnection', profile),
  setAutostart: (enabled) => ipcRenderer.invoke('setAutostart', enabled),
});
