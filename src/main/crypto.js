const crypto = require('crypto');

/**
 * Windows DPAPI 加密/解密（当前用户作用域）
 * 使用 win-dpapi 原生模块调用 CryptProtectData/CryptUnprotectData
 * 回退：AES-256-GCM（当 win-dpapi 不可用时）
 */

let dpapi = null;
try {
  dpapi = require('win-dpapi');
} catch (e) {
  console.warn('win-dpapi not available, falling back to AES-256-GCM');
}

class Crypto {
  /**
   * 加密文本（DPAPI 优先，回退 AES-256-GCM）
   * @param {string} text - 明文
   * @returns {object} 加密结果
   */
  static encrypt(text) {
    try {
      if (dpapi) {
        const encrypted = dpapi.protect(text);
        // DPAPI 返回 Buffer，转为 base64 存储
        return {
          encrypted: encrypted.toString('base64'),
          algorithm: 'dpapi',
        };
      }
      // 回退：AES-256-GCM
      const key = crypto.randomBytes(32);
      const iv = crypto.randomBytes(16);
      const cipher = crypto.createCipheriv('aes-256-gcm', key, iv);
      let encrypted = cipher.update(text, 'utf8', 'hex');
      encrypted += cipher.final('hex');
      const authTag = cipher.getAuthTag();
      return {
        encrypted,
        key: key.toString('hex'),
        iv: iv.toString('hex'),
        authTag: authTag.toString('hex'),
        algorithm: 'aes-256-gcm',
      };
    } catch (e) {
      console.error('Encryption failed:', e);
      throw new Error('Encryption failed');
    }
  }

  /**
   * 解密文本
   * @param {string} encrypted - 密文（DPAPI: base64, AES: hex）
   * @param {string} key - 密钥 hex（AES 模式需要）
   * @param {string} iv - IV hex（AES 模式需要）
   * @param {string} authTag - 认证标签 hex（AES 模式需要）
   * @returns {string} 明文
   */
  static decrypt(encrypted, key, iv, authTag) {
    try {
      if (dpapi) {
        // DPAPI 模式：encrypted 是 base64
        const buffer = Buffer.from(encrypted, 'base64');
        return dpapi.unprotect(buffer);
      }
      // AES-256-GCM 模式
      const decipher = crypto.createDecipheriv(
        'aes-256-gcm',
        Buffer.from(key, 'hex'),
        Buffer.from(iv, 'hex')
      );
      decipher.setAuthTag(Buffer.from(authTag, 'hex'));
      let decrypted = decipher.update(encrypted, 'hex', 'utf8');
      decrypted += decipher.final('utf8');
      return decrypted;
    } catch (e) {
      console.error('Decryption failed:', e);
      throw new Error('Decryption failed');
    }
  }

  /**
   * 使用 Windows DPAPI 加密（明确调用）
   * @param {string} text - 明文
   * @returns {object} 加密结果 { encrypted, algorithm }
   */
  static encryptWithDPAPI(text) {
    if (!dpapi) {
      throw new Error('win-dpapi not available. Install it with: npm install win-dpapi');
    }
    const encrypted = dpapi.protect(text);
    return {
      encrypted: encrypted.toString('base64'),
      algorithm: 'dpapi',
    };
  }

  /**
   * 使用 Windows DPAPI 解密（明确调用）
   * @param {string} encrypted - 密文 base64
   * @returns {string} 明文
   */
  static decryptWithDPAPI(encrypted) {
    if (!dpapi) {
      throw new Error('win-dpapi not available. Install it with: npm install win-dpapi');
    }
    const buffer = Buffer.from(encrypted, 'base64');
    return dpapi.unprotect(buffer);
  }
}

module.exports = { Crypto };
