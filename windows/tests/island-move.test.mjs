import { test } from 'node:test';
import assert from 'node:assert/strict';
import { IslandMove } from '../src/island/move.ts';

const point = (clientX, clientY, screenX = clientX, screenY = clientY, overlay = false) => ({ clientX, clientY, screenX, screenY, overlay });
const deferred = () => { let resolve; const promise = new Promise(r => { resolve = r; }); return { promise, resolve }; };
const flush = async () => { for (let i = 0; i < 20; i++) await Promise.resolve(); };
function setup(info) {
  const events = [];
  const begin = deferred();
  const move = new IslandMove({
    begin: () => begin.promise,
    move: async p => { events.push(['move', p]); },
    end: async p => { events.push(['end', p]); },
    paint: (p, i) => { events.push(['paint', p, i?.mode]); },
  });
  if (info) begin.resolve(info);
  return { move, begin, events };
}
const layer = { mode: 'layer', x: 600, y: 100, scale: 1 };
const x11 = { mode: 'window', x: -1200, y: 200, scale: 2 };

test('a layer drop uses stationary overlay coordinates and explicit release exactly once', async () => {
  const { move, events } = setup(layer);
  move.start(point(350, 15), point(360, 25));
  await flush();
  move.update(point(1000, 415, 0, 0, true));
  move.finish(point(1050, 465, 0, 0, true));
  move.finish(point(1050, 465, 0, 0, true));
  await flush();
  assert.deepEqual(events.filter(e => e[0] === 'end'), [['end', { x: 700, y: 450 }]]);
  assert.equal(move.active, false);
});

test('a release before begin resolves is preserved instead of leaving a live drag', async () => {
  const { move, begin, events } = setup();
  move.start(point(350, 15), point(365, 35));
  move.finish(point(370, 45));
  begin.resolve(layer);
  await flush();
  assert.deepEqual(events.filter(e => e[0] === 'end'), [['end', { x: 620, y: 130 }]]);
  assert.equal(move.active, false);
});

test('lost capture, Escape or pointercancel cancels even during the begin round trip', async () => {
  const { move, begin, events } = setup();
  move.start(point(350, 15), point(365, 35));
  move.cancel();
  begin.resolve(layer);
  await flush();
  assert.deepEqual(events.filter(e => e[0] === 'end'), [['end', null]]);
  assert.equal(move.active, false);
});

test('X11 follows desktop coordinates across monitors and scales logical deltas once', async () => {
  const { move, events } = setup(x11);
  move.start(point(350, 15, -250, 115), point(365, 35, -235, 135));
  await flush();
  move.update(point(350, 15, 950, 415));
  await flush();
  move.finish(point(350, 15, 1050, 515));
  await flush();
  assert.deepEqual(events.filter(e => e[0] === 'end'), [['end', { x: 1400, y: 1000 }]]);
  assert.ok(events.some(e => e[0] === 'move' && e[1].x === 1200));
});

test('a failed begin clears the gesture and a repeated drag can start', async () => {
  const { move, begin, events } = setup();
  move.start(point(10, 10), point(20, 20));
  begin.resolve(null);
  await flush();
  assert.equal(move.active, false);
  assert.equal(events.filter(e => e[0] === 'end').length, 0);
  assert.equal(move.start(point(10, 10), point(20, 20)), true);
  await flush();
});

test('an in-flight X11 move cannot arrive after release or overwrite the final point', async () => {
  const moving = deferred(), events = [];
  const move = new IslandMove({
    begin: async () => x11,
    move: async p => { events.push(['move', p]); await moving.promise; },
    end: async p => { events.push(['end', p]); },
    paint() {},
  });
  move.start(point(350, 15, 0, 0), point(360, 25, 10, 10));
  await flush();
  move.update(point(350, 15, 20, 20));
  move.finish(point(350, 15, 50, 50));
  assert.equal(move.start(point(0, 0), point(10, 10)), false, 'do not overlap a finishing gesture');
  await flush();
  assert.equal(events.length, 1, 'release waits for the outstanding move');
  moving.resolve();
  await flush();
  assert.deepEqual(events.at(-1), ['end', { x: -1100, y: 300 }]);
  assert.equal(events.length, 2, 'intermediate moves are coalesced');
});

test('a positioning failure restores the bounded window without persisting the partial drag', async () => {
  const events = [];
  const move = new IslandMove({
    begin: async () => x11,
    move: async () => { throw new Error('window move failed'); },
    end: async p => { events.push(p); },
    paint() {},
  });
  move.start(point(350, 15), point(370, 25));
  await flush();
  assert.deepEqual(events, [null]);
  assert.equal(move.active, false);
});

test('begin rejection and a closed window do not leave active frontend state', async () => {
  const move = new IslandMove({
    begin: async () => { throw new Error('closed'); },
    move: async () => {},
    end: async () => { throw new Error('closed'); },
    paint() {},
  });
  move.start(point(350, 15), point(370, 25));
  await flush();
  assert.equal(move.active, false);
});
