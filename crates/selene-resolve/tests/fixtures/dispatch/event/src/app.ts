import { bus } from './bus';
import { initApp } from './init';

bus.on('mount', function onmount() {
  initApp();
});

// The anonymous frontier: this must synthesize NOTHING (see the test).
bus.on('tick', () => refresh());

export class Application {
  use() {
    bus.emit('mount');
  }
}

function refresh() {
  return 1;
}
