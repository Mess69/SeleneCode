import { renderStaticScene } from './renderer';

export function StaticCanvas(props: any) {
  return <canvas>{renderStaticScene(props.scene)}</canvas>;
}
