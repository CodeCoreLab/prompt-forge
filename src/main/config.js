const { app } = require('electron');
const path = require('path');
const fs = require('fs');

class ConfigManager {
  constructor() {
    this.configPath = this.getConfigPath();
    this.config = this.load();
  }

  getConfigPath() {
    const home = require('os').homedir();
    const dir = path.join(home, 'AppData', 'Roaming', 'prompt-forge');
    fs.mkdirSync(dir, { recursive: true });
    return path.join(dir, 'config.json');
  }

  load() {
    try {
      if (fs.existsSync(this.configPath)) {
        const content = fs.readFileSync(this.configPath, 'utf-8');
        return JSON.parse(content);
      }
    } catch (e) {
      console.error('Failed to load config:', e);
    }
    return {
      profiles: [],
      active_index: 0,
      close_to_tray: true,
      autostart: true,
      custom_modes: [],
    };
  }

  save(config) {
    try {
      fs.writeFileSync(this.configPath, JSON.stringify(config, null, 2));
      this.config = config;
    } catch (e) {
      console.error('Failed to save config:', e);
      throw e;
    }
  }

  getConfig() {
    return this.config;
  }

  addModel(model) {
    if (this.config.profiles.some(m => m.name === model.name)) {
      throw new Error(`模型配置「${model.name}」已存在`);
    }
    this.config.profiles.push(model);
    this.save(this.config);
  }

  removeModel(index) {
    if (index >= 0 && index < this.config.profiles.length) {
      this.config.profiles.splice(index, 1);
      if (this.config.active_index >= this.config.profiles.length) {
        this.config.active_index = 0;
      }
      this.save(this.config);
    }
  }
}

module.exports = { ConfigManager };
