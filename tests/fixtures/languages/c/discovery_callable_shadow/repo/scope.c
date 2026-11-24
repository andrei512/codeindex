void marker(void) {}
void shadow(void (*marker)(void)) { marker(); }
void positive(void) { marker(); }
