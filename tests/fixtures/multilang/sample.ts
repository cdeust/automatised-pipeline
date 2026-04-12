import { Request, Response } from 'express';

export const MAX_RETRIES = 3;

export function greet(name: string): string {
    return `Hello, ${name}`;
}

export class Config {
    public host: string;
    public port: number;

    constructor(host: string, port: number) {
        this.host = host;
        this.port = port;
    }

    url(): string {
        return `http://${this.host}:${this.port}`;
    }
}

export interface Serializable {
    serialize(): string;
}

export type StringOrNum = string | number;
