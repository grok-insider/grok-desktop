export async function withStartupDeadline<T>(
  operation: Promise<T>,
  timeoutMs: number,
): Promise<T> {
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 30_000) {
    throw new Error("invalid startup deadline");
  }
  let deadline: NodeJS.Timeout | undefined;
  const timeout = new Promise<never>((_resolve, reject) => {
    deadline = setTimeout(() => reject(new Error("startup deadline exceeded")), timeoutMs);
  });
  try {
    return await Promise.race([operation, timeout]);
  } finally {
    if (deadline) clearTimeout(deadline);
  }
}
