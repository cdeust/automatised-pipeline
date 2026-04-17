// Synthetic corpus file — exercises TS interface + class + method extraction.

export interface Logger {
    log(message: string): void;
    warn(message: string): void;
}

export class ConsoleLogger implements Logger {
    private prefix: string;

    constructor(prefix: string) {
        this.prefix = prefix;
    }

    log(message: string): void {
        this.write("info", message);
    }

    warn(message: string): void {
        this.write("warn", message);
    }

    private write(level: string, message: string): void {
        console.log(this.prefix + " [" + level + "] " + message);
    }
}

export class NullLogger implements Logger {
    log(_message: string): void {}
    warn(_message: string): void {}
}
