import { render } from './ui';

export class App {
  start = () => {
    boot();
  };

  stop() {
    teardown();
  }
}

function boot() {
  render();
}

export default App;
