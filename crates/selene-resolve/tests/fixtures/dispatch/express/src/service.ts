import { hashPassword } from './crypto';

export async function login(body: any) {
  const hashed = hashPassword(body.password);
  return { token: hashed };
}
