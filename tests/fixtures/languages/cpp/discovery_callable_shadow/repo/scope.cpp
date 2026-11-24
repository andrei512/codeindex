void marker() {}
void shadow(void (*marker)()) { marker(); }
void positive() { marker(); }
