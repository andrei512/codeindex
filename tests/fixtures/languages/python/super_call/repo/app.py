class Base:
    def save(self): pass
class Other:
    def save(self): pass
class Child(Base):
    def run(self):
        super().save()
