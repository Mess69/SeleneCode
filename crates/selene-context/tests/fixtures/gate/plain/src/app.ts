import { login } from './service';

export function handleLogin(user: string) {
  return login(user);
}
