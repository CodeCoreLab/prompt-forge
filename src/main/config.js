const path = require('path');

/**
 * 配置访问层：唯一真源是 Rust CLI（config.json）。
 *
 * 原因：Rust 侧负责 DPAPI 加密/解密与历史记录落盘；Node 侧若自行读写
 * 同一文件会覆盖历史并写出明文 Key。故此处只做 CLI 代理 + 同步缓存。
 */
class ConfigManager {
  /** @param {(action: string, payload?: object) => Promise<any>} sendToCli */
  constructor(sendToCli) {
    this.sendToCli = sendToCli;
    this.config = null;
  }

  async refresh() {
    this.config = await this.sendToCli('get_config');
    return this.config;
  }

  async getConfig() {
    if (!this.config) await this.refresh();
    return this.config;
  }

  /** 同步读取缓存（窗口事件等非 async 上下文用） */
  current() {
    return this.config;
  }

  /** 保存：Rust 侧加密 Key + 校验 + 原子写 */
  async save(config) {
    await this.sendToCli('save_config', config);
    this.config = config;
    return this.config;
  }
}

module.exports = { ConfigManager };
