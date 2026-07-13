
export function processPayment(amount: number): Promise<Receipt> {
  return stripe.charge(amount);
}
