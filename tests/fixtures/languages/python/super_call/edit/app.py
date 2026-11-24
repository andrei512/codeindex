class Base:
    def save(self): pass
class Other:
    def save(self): pass
class Child(Other):
    def run(self):
        super().save()
