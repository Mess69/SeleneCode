export function hashPassword(pw: string): string {
  return pw + 'salt';
}
