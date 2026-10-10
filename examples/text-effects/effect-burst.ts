// A bounded, reusable latest-wins queue for MUD attention effects.
// Drain from one low-frequency timer; animation itself runs on the GPU.
export function createEffectBurst<T>(capacity = 64, budget = 3, cooldownMs = 1200) {
    if (![capacity, budget, cooldownMs].every(Number.isInteger) || capacity < 1 || budget < 1 || cooldownMs < 0) throw new TypeError("Invalid effect burst limits");
    const pending = new Map<string, T>();
    const last = new Map<string, number>();
    let previousTime = -Infinity;
    return {
        enqueue(key: string, value: T) {
            if (!pending.has(key) && pending.size >= capacity) pending.delete(pending.keys().next().value!);
            pending.set(key, value);
        },
        drain(now: number, play: (value: T) => void) {
            if (!Number.isFinite(now)) throw new TypeError("Effect clock must be finite");
            if (now < previousTime) last.clear();
            previousTime = now;
            // Cooldown entries expire even when no further message mentions that target.
            for (const [key, time] of last) if (now - time >= cooldownMs) last.delete(key);
            let count = 0;
            for (const [key, value] of pending) {
                if (count >= budget) break;
                pending.delete(key);
                if (last.has(key)) continue;
                // Mark first so reentrant producers cannot bypass the cooldown.
                last.set(key, now);
                if (last.size > capacity) last.delete(last.keys().next().value!);
                play(value); count++;
            }
            return count;
        },
        clear() { pending.clear(); last.clear(); },
        get size() { return pending.size; },
    };
}
