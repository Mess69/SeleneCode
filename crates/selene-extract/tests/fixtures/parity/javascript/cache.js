const path = require('path');
import { Logger } from './logger';

export class Cache {
  constructor(size) {
    this.size = size;
    this.logger = new Logger();
  }

  get(key) {
    this.logger.debug(key);
    return normalize(key);
  }
}

export function normalize(key) {
  return path.basename(key);
}
