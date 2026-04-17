// Synthetic corpus file — application entry. Imports and wires the others.

import { ConsoleLogger, Logger } from "./logger";
import { MemoryStore } from "./store";

export class App {
    private logger: Logger;
    private store: MemoryStore<string>;

    constructor() {
        this.logger = new ConsoleLogger("app");
        this.store = new MemoryStore<string>(this.logger);
    }

    run(): void {
        this.store.set("hello", "world");
        const v = this.store.get("hello");
        this.logger.log("got " + (v ?? "null"));
    }
}

export function main(): void {
    const app = new App();
    app.run();
}
