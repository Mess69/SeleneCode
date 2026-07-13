type Handler = (...args: any[]) => void;

export class Bus {
  private map = new Map<string, Handler[]>();

  on(event: string, h: Handler) {
    const list = this.map.get(event) || [];
    list.push(h);
    this.map.set(event, list);
  }

  emit(event: string) {
    for (const h of this.map.get(event) || []) {
      h();
    }
  }
}

export const bus = new Bus();
