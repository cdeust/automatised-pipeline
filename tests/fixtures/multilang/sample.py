import os
from pathlib import Path

MAX_RETRIES = 3

def greet(name: str) -> str:
    return f"Hello, {name}"

def add(a: int, b: int) -> int:
    return a + b

class Config:
    def __init__(self, host: str, port: int):
        self.host = host
        self.port = port

    def url(self) -> str:
        return f"http://{self.host}:{self.port}"
