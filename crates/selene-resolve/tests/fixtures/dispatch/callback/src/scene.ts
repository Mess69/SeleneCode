type Callback = () => void;

export class Scene {
  private callbacks = new Set<Callback>();

  onUpdate(cb: Callback) {
    this.callbacks.add(cb);
  }

  triggerUpdate() {
    for (const cb of this.callbacks) {
      cb();
    }
  }
}
