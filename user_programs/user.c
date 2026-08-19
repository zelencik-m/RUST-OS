// user.c
// A read-only string to test .rodata copying
const char test_string[] = "Hello from C!";

// An uninitialized array to test .bss zeroing. 
// The linker script will force this to exist exactly at virtual address 0x401000.
volatile unsigned long bss_array[256] __attribute__((section(".magic")));

void _start(void) {
    // Simple computation to ensure it's a real program
    int x = 0;
    for(int i=0; i<10; i++) {
        x += i;
    }
    
    // Infinite loop (prevents compiler from optimizing away variables)
    while(1) {
        x++;
    }
}