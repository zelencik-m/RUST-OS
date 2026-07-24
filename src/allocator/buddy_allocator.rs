use alloc::alloc::{GlobalAlloc, Layout};

use super::{Locked, align_up};
use core::{ptr::null_mut};
use core::mem;

use super::super::{println, print};

struct ListNode {
    next: Option<&'static mut ListNode>,
}

const MAX_ORDER:usize = 30;
const MIN_ORDER:usize = 4;
const NUM_ORDERS: usize = MAX_ORDER - MIN_ORDER +1;

pub struct BuddyAllocator{
    heap_start: usize,
    heap_size: usize,

    free_lists: [Option<&'static mut ListNode>; NUM_ORDERS], 
}

impl BuddyAllocator{
    pub const fn new() -> Self{
        const EMPTY: Option<&'static mut ListNode> = None;

        BuddyAllocator{
            heap_start: 0,
            heap_size: 0,

            free_lists: [EMPTY; NUM_ORDERS],
        }
    }

    pub unsafe fn init(&mut self, heap_start: usize, heap_size: usize) {
        for i in 0..NUM_ORDERS {
            self.free_lists[i] = None;
        }

        println!("Heap start (raw): {:#x}", heap_start);
        println!("Heap size (raw):  {} bytes", heap_size);

        if heap_size == 0 {
            panic!("Heap size cannot be zero");
        }

        let max_possible = heap_size.next_power_of_two() >> 1;
        let mut candidate_size = if heap_size.is_power_of_two() {
            heap_size
        } else {
            max_possible
        };
        candidate_size = candidate_size.min(1 << MAX_ORDER);

        let (aligned_start, adjusted_size) = loop {
            if candidate_size < (1 << MIN_ORDER) {
                panic!("Heap too small: cannot fit even a minimum-order block after alignment");
            }

            let aligned = align_up(heap_start, candidate_size);
            let end = heap_start.saturating_add(heap_size);

            if aligned <= end && candidate_size <= end - aligned {
                break (aligned, candidate_size);
            }

            candidate_size >>= 1;
        };

        let order = match self.size_to_order(adjusted_size) {
            Some(o) => o,
            None => panic!("Heap size {} out of bounds", adjusted_size),
        };

        debug_assert_eq!(aligned_start % adjusted_size, 0);

        println!("Heap start (aligned): {:#x}", aligned_start);
        println!("Heap size (adjusted): {} bytes (order {})", adjusted_size, order);

        self.heap_start = aligned_start;
        self.heap_size = adjusted_size;

        self.push_free_block(order, aligned_start as *mut u8);

        self.print_free_lists();
    }
}

unsafe impl GlobalAlloc for Locked<BuddyAllocator>{
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let mut buddy_allocator = self.lock();

        let required_block_size = layout.size().max(layout.align());
        let required_order = match buddy_allocator.size_to_order(required_block_size) {
            Some(o) => o,
            None => return null_mut(),
        };
        
        let mut order = required_order;

        while order < NUM_ORDERS && buddy_allocator.free_lists[order].is_none() {
            order += 1;
        }

        if order >= NUM_ORDERS {
            return null_mut();
        }


        let block = buddy_allocator.pop_free_block(order);

        while order > required_order {
            order -= 1;
            let half_size = buddy_allocator.order_to_size(order);
            let buddy = (block as usize + half_size) as *mut u8;
            buddy_allocator.push_free_block(order, buddy);
        }

        block
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let mut buddy_allocator = self.lock();

        let required_block_size = layout.size().max(layout.align());
        let required_order = match buddy_allocator.size_to_order(required_block_size) {
            Some(o) => o,
            None => return,
        };

        let mut current_address: usize = ptr as usize; 
        let mut current_order = required_order;

        while let Some(buddy_ptr) = buddy_allocator.pop_buddy_of(current_address, current_order){
            let buddy_address = buddy_ptr as usize;
            current_address = current_address.min(buddy_address);
            current_order +=1;
            if current_order >= NUM_ORDERS{
                break;
            }
        }

        buddy_allocator.push_free_block(current_order, current_address as *mut u8);

        
    }
}


impl BuddyAllocator {
    fn size_to_order(&self, size: usize) -> Option<usize> {
        if size == 0 {
            return Some(0);
        }

        let mut block_size = size;

        if block_size < (1 << MIN_ORDER) {
            return Some(0);
        }

        block_size = block_size.next_power_of_two();

        if block_size > (1 << MAX_ORDER) {
            return None;
        }

        let order = block_size.trailing_zeros() as usize - MIN_ORDER;
        Some(order)
    }

    fn order_to_size(&self, order: usize) -> usize {
        1 << (MIN_ORDER + order)
    }

    fn buddy_of(&self, address: usize, order: usize) -> usize {
        let block_size = self.order_to_size(order);
        address ^ block_size
    }

    unsafe fn push_free_block(&mut self, order: usize, ptr: *mut u8) {
        assert!(order < NUM_ORDERS, "push_free_block: order out of bounds");

        let node = &mut *(ptr as *mut ListNode);

        node.next = self.free_lists[order].take();
        self.free_lists[order] = Some(node);
    }

    unsafe fn pop_free_block(&mut self, order: usize) -> *mut u8 {
        assert!(order < NUM_ORDERS, "pop_free_block: order out of bounds");

        match self.free_lists[order].take() {
            Some(node) => {
                self.free_lists[order] = node.next.take();
                node as *mut ListNode as *mut u8
            }
            None => null_mut(),
        }
    }

    unsafe fn pop_buddy_of(&mut self, address: usize, order: usize) -> Option<*mut u8> {
        assert!(order < NUM_ORDERS, "pop_buddy_of: order out of bounds");

        let buddy_addr = self.buddy_of(address, order);
        
        if let Some(node) = self.free_lists[order].as_ref() {
            if *node as *const ListNode as usize == buddy_addr {
                return Some(self.pop_free_block(order));
            }
        }
        
        let mut prev: *mut ListNode = match self.free_lists[order].as_mut() {
            Some(node) => *node as *mut ListNode,
            None => return None,
        };
        
        loop {
            let current: *mut ListNode = match (*prev).next.as_mut() {
                Some(node) => *node as *mut ListNode,
                None => return None,
            };
            
            if current as usize == buddy_addr {
                let current_ref = &mut *current;
                (*prev).next = current_ref.next.take();
                return Some(current as *mut u8);
            }
            
            prev = current;
        }
    }

    fn print_free_lists(&self) {
        println!("========== Buddy Allocator Free Lists ==========");
        println!("Heap: start={:#x}, size={} bytes", self.heap_start, self.heap_size);
        
        let mut total_blocks = 0;
        
        for order in 0..NUM_ORDERS {
            let block_size = self.order_to_size(order);
            let mut count = 0;
            
            // Traverse using raw pointers to avoid modifying the list
            let mut current: *const ListNode = match self.free_lists[order].as_ref() {
                Some(node) => *node as *const ListNode,
                None => core::ptr::null(),
            };
            
            if !current.is_null() {
                print!("Order {:2} (size={:8} bytes): ", order, block_size);
                
                while !current.is_null() {
                    count += 1;
                    print!("{:#x} ", current as usize);
                    current = match unsafe { (*current).next.as_ref() } {
                        Some(node) => *node as *const ListNode,
                        None => core::ptr::null(),
                    };
                }
                
                println!("({} blocks)", count);
                total_blocks += count;
            }
        }
        
        if total_blocks == 0 {
            println!("No free blocks available.");
        }
        
        println!("================================================");
    }

}