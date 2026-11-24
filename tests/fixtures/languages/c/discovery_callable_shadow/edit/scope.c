void marker(void) {}
void shadow(void (*action)(void)) { marker(); }
void positive(void) { marker(); }
