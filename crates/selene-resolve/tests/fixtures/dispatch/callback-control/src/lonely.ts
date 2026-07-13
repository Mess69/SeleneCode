export class Lonely {
  private things = new Set<string>();

  onThing(t: string) {
    this.things.add(t);
  }
}

export class Shouter {
  emitAll() {
    for (const x of this.other) {
      x();
    }
  }
  private other: any[] = [];
}
