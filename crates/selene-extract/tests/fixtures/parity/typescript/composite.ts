import { Stripe } from 'stripe';
import type { Receipt } from './types';

export interface Ledger {
  id: string;
}

export type Currency = 'usd' | 'eur';

export class BillingService {
  private client = new Stripe();
  handler = makeHandler();

  async settle(amount: number): Promise<Receipt> {
    const r = await this.client.charge(amount);
    return normalize(r);
  }
}

export const format = (r: Receipt): string => r.id;

function normalize(r: Receipt): Receipt {
  return r;
}

const MAX_AMOUNT = 10000;
