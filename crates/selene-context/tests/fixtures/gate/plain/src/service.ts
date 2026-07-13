import { hashPassword } from './crypto';

export function login(user: string) {
  const hashed = hashPassword(user);
  return hashed;
}
