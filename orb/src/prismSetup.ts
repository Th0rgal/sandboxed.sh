// Prism must not install its own JSON worker protocol or scan the DOM.
(globalThis as unknown as {Prism?:object}).Prism = {manual:true,disableWorkerMessageHandler:true};
