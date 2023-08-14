use proc_macro2::TokenStream;
use quote::quote;

/// Generate the wrapper function that calls a win32api function by taking arguments using from_x86.
///
/// winapi is stdcall, which means args are pushed right to left.
/// They are callee-cleaned which means they are popped left to right, as done here.
/// The caller of winapi functions is responsible for pushing/popping the
/// return address, because some callers actually 'jmp' directly.
///
/// This macro generates shim wrappers of functions, taking their
/// input args off the stack and forwarding their return values via eax.
pub fn fn_wrapper(
    module: TokenStream,
    ordinal: Option<usize>,
    func: &syn::ItemFn,
) -> (TokenStream, TokenStream) {
    let mut args = Vec::new();
    let mut tys = Vec::new();

    // Skip first arg, the &Machine.
    for arg in func.sig.inputs.iter().skip(1) {
        let arg = match arg {
            syn::FnArg::Typed(arg) => arg,
            _ => unimplemented!(),
        };
        let name = match arg.pat.as_ref() {
            syn::Pat::Ident(ident) => &ident.ident,
            _ => unimplemented!(),
        };
        args.push(name);
        tys.push(&arg.ty);
    }

    let name = &func.sig.ident;
    let name_str = name.to_string();

    let fetch_args = quote! {
        // We expect all the stack_offset math to be inlined by the compiler into plain constants.
        // TODO: reading the args in reverse would produce fewer bounds checks...
        let mut stack_offset = 4u32;
        #(
            let #args = unsafe { <#tys>::from_stack(machine.mem(), esp + stack_offset) };
            stack_offset += <#tys>::stack_consumed();
        )*
    };
    let ret = quote! {
        machine.x86.cpu.regs.eax = result.to_raw();
        machine.x86.cpu.regs.eip = machine.mem().get::<u32>(esp);
        machine.x86.cpu.regs.esp += stack_offset;
    };

    // If the function is async, we need to handle the return value a bit differently.
    let body = if func.sig.asyncness.is_some() {
        quote! {
            #fetch_args
            // Yuck: we know Machine will outlive the future, but Rust doesn't.
            // At least we managed to isolate the yuck to this point.
            let m: *mut Machine = machine;
            let result = async move {
                let machine = unsafe { &mut *m };
                let result = #module::#name(machine, #(#args),*).await;
                #ret
            };
            crate::shims::become_async(machine, Box::pin(result));
            // push_async will set up the stack and eip.
        }
    } else {
        quote! {
            #fetch_args
            let result = #module::#name(machine, #(#args),*);
            #ret
        }
    };

    let ordinal_tok = match ordinal {
        None => quote!(None),
        Some(o) => quote!(Some(#o)),
    };

    let stack_consumed = if tys.is_empty() {
        quote!(0)
    } else {
        quote!(#(<#tys>::stack_consumed())+*)
    };

    (
        quote!(pub fn #name(machine: &mut Machine, esp: u32) { #body }),
        quote!(Symbol { name: #name_str, ordinal: #ordinal_tok, func: #name, stack_consumed: || { #stack_consumed } }),
    )
}

/// Insert a `log::info!` at the front of a function that logs its name and arguments.
// TODO: this fn is used by lib.rs, but not main.rs.
#[allow(dead_code)]
pub fn add_trace(func: &mut syn::ItemFn) {
    let mut fmt: String = "{}/".into(); // TRACE_CONTEXT prefix
    fmt.push_str(&func.sig.ident.to_string());
    let mut args: Vec<&syn::Ident> = Vec::new();
    for arg in func.sig.inputs.iter().skip(1) {
        match arg {
            syn::FnArg::Typed(arg) => match &*arg.pat {
                syn::Pat::Ident(pat) => {
                    args.push(&pat.ident);
                }
                _ => {}
            },
            _ => {}
        };
    }
    fmt.push_str("(");
    fmt.push_str(
        &args
            .iter()
            .map(|arg| format!("{arg}:{{:x?}}"))
            .collect::<Vec<_>>()
            .join(", "),
    );
    fmt.push_str(")");
    let stmt = syn::parse_quote! {
        if crate::trace::enabled(TRACE_CONTEXT) { log::info!(#fmt, TRACE_CONTEXT, #(#args),*); }
    };
    func.block.stmts.insert(0, stmt);
}
