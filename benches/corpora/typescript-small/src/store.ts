// Synthetic corpus file — exercises interface + generic class + cross-file calls.

import { Logger } from "./logger";

export interface Store<T> {
    get(key: string): T | undefined;
    set(key: string, value: T): void;
    delete(key: string): boolean;
}

export class MemoryStore<T> implements Store<T> {
    private data: Map<string, T>;
    private logger: Logger;

    constructor(logger: Logger) {
        this.data = new Map();
        this.logger = logger;
    }

    get(key: string): T | undefined {
        this.logger.log("get " + key);
        return this.data.get(key);
    }

    set(key: string, value: T): void {
        this.logger.log("set " + key);
        this.data.set(key, value);
    }

    delete(key: string): boolean {
        this.logger.log("delete " + key);
        return this.data.delete(key);
    }

    size(): number {
        return this.data.size;
    }
}
