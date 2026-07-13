
// plain class control
class Ledger {}

// exported class
export class Invoice {}

// export default
export default function settle() { return true; }

// exported arrow const
export const refund = (amount: number) => amount;

// non-export arrow const
const audit = (amount: number) => amount;
