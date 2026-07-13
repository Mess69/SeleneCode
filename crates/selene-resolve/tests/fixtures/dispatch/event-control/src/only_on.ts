import { bus } from './bus';

bus.on('never', function neverFires() {
  doThing();
});

function doThing() {
  return 1;
}
