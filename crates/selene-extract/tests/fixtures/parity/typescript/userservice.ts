import { Logger } from './logger';
import * as fs from 'fs';

export interface Repo {
  find(id: string): User | null;
}

export class UserService implements Repo {
  private readonly logger: Logger;

  constructor(logger: Logger) {
    this.logger = new Logger('users');
  }

  find(id: string): User | null {
    this.logger.debug(id);
    return loadUser(id);
  }
}

export function loadUser(id: string): User | null {
  const raw = fs.readFileSync(id, 'utf8');
  return JSON.parse(raw);
}
