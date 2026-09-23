// MSVC STL ABI 垫片（Phase 2 F02）
//
// heif-rs 预编译的 heif.lib 由 MSVC 14.45+ 构建，引用了 STL 向量化辅助函数
// __std_rotate / __std_max_element_4i / __std_unique_4；本机 Build Tools 14.44
// 的 STL 尚无这些符号，链接报 LNK2019。此处按 STL ABI 语义手工补齐，
// 与 MSVC 后续版本内置实现行为等价。
//
// Build Tools 14.45+ 已经自带这些符号，不能再定义一份同名的 extern "C"
// 实现，否则新版 STL 头文件和链接阶段都会报重复定义。因此保留本文件和
// build.rs 的统一入口，但只在旧工具链（< 14.45）上编译下面的实现。

#if !defined(_MSC_VER) || _MSC_VER < 1945

#include <algorithm>
#include <cstdint>
#include <cstring>

namespace {

// 字节级交换（__std_rotate 以元素字节宽度为参数）
inline void swap_bytes(void* a, void* b, size_t n) {
    char* pa = static_cast<char*>(a);
    char* pb = static_cast<char*>(b);
    for (size_t i = 0; i < n; ++i) {
        char t = pa[i];
        pa[i] = pb[i];
        pb[i] = t;
    }
}

} // namespace

// STL std::rotate 的库内辅助：将 [first,last) 绕 middle 旋转（元素宽度 elem_size 字节）
extern "C" void* __std_rotate(void* first, void* middle, void* last, size_t elem_size) {
    char* f = static_cast<char*>(first);
    char* m = static_cast<char*>(middle);
    char* l = static_cast<char*>(last);
    size_t n = static_cast<size_t>(l - f);
    size_t k = static_cast<size_t>(m - f);
    if (k == 0 || k == n) {
        return m;
    }
    // 三轮反转法（与 STL 语义一致：返回旋转后原 middle 元素的新位置）
    size_t es = elem_size;
    auto rev = [es](char* lo, char* hi) {
        size_t len = static_cast<size_t>(hi - lo);
        for (size_t i = 0; i < len / 2; i += es) {
            swap_bytes(lo + i, hi - es - i, es);
        }
    };
    rev(f, m);
    rev(m, l);
    rev(f, l);
    return f + (n - k);
}

// STL std::max_element 的 int32 向量化辅助：返回最大元素指针
extern "C" int* __std_max_element_4i(int* first, int* last) {
    if (first == last) {
        return last;
    }
    return std::max_element(first, last);
}

// STL std::unique 的 u32 向量化辅助：原地去除升序范围中重复值，返回新尾指针
extern "C" uint32_t* __std_unique_4(uint32_t* first, uint32_t* last) {
    if (first == last) {
        return last;
    }
    uint32_t* out = first;
    for (uint32_t* p = first + 1; p < last; ++p) {
        if (*p != *out) {
            *++out = *p;
        }
    }
    return out + 1;
}

#endif // !defined(_MSC_VER) || _MSC_VER < 1945
