const crypto = require('crypto');

class Crypto {
  static encrypt(text) {
    try {
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
      };
    } catch (e) {
      console.error('Encryption failed:', e);
      throw new Error('Encryption failed');
    }
  }

  static decrypt(encrypted, key, iv, authTag) {
    try {
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

  // 使用 Windows DPAPI（通过 Node.js 原生模块或 system-protocols）
  static encryptWithDPAPI(text) {
    // TODO: 实现 DPAPI 加密
    // 当前使用 AES-256-GCM 作为替代
    return this.encrypt(text);
  }

  static decryptWithDPAPI(encrypted, key, iv, authTag) {
    // TODO: 实现 DPAPI 解密
    return this.decrypt(encrypted, key, iv, authTag);
  }
}

module.exports = { Crypto };
