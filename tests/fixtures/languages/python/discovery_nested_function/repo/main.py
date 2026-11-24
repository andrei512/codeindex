def send():
    return "outer"
def run():
    def send():
        return "inner"
    return send()
print(run(), send())
