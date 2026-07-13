import { Scene } from './scene';
import { renderScene } from './renderer';

export class App {
  private scene: Scene;

  constructor() {
    this.scene = new Scene();
    this.scene.onUpdate(this.triggerRender);
  }

  triggerRender() {
    renderScene();
  }

  mutateElement() {
    this.scene.triggerUpdate();
  }
}
