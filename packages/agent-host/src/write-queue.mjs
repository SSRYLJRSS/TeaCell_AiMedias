function writeWithBackpressure(stream, chunk) {
  return new Promise((resolve, reject) => {
    let settled = false;
    let backpressured = false;
    let drainSeen = false;

    const cleanup = () => {
      stream.removeListener('drain', onDrain);
      stream.removeListener('error', onError);
    };
    const finish = error => {
      if (settled) return;
      settled = true;
      cleanup();
      if (error) reject(error);
      else resolve();
    };
    const onDrain = () => {
      if (backpressured) finish();
      else drainSeen = true;
    };
    const onError = error => finish(error);

    stream.once('drain', onDrain);
    stream.once('error', onError);
    try {
      if (stream.write(chunk)) {
        finish();
      } else {
        backpressured = true;
        if (drainSeen) finish();
      }
    } catch (error) {
      finish(error);
    }
  });
}

/** Keep protocol frames ordered and pause the producer until a blocked stream drains. */
export function createWriteQueue(stream) {
  let tail = Promise.resolve();

  return {
    write(chunk) {
      tail = tail.then(() => writeWithBackpressure(stream, chunk));
      return tail;
    },
    flush() {
      return tail;
    },
  };
}
