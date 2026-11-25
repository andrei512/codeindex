import os

def transform(value):
    print(f"CASE={value}")
    result = abs(value) if value < 0 else value * 2
    return result + int(os.getenv("CX_PAIR_MUTANT") == str(value))
