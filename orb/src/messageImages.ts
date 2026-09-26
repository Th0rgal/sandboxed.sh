/** Interpret transport markers only at the display boundary. */
export function messageImages(text: string): {text:string; paths:string[]; references:number[]} {
  const paths:string[]=[];
  const references:number[]=[];
  const visible=text.replace(/(?:\[Image #(\d+)\][ \t]*)?\[Uploaded: ([^\]\r\n]+)\]/gi,(marker,label:string|undefined,path:string,offset:number)=>{
    const inline = /^data:image\/(?:png|jpeg|webp|gif);base64,[A-Za-z0-9+/]+=*$/i.test(path);
    if (!inline && (!/^(?:\/|~\/|\.\.?\/)/.test(path) || !/\.(?:png|jpe?g|webp|gif)$/i.test(path))) return marker;
    let index=paths.indexOf(path);
    if(index<0){index=paths.length;paths.push(path);references.push(label ? Number(label) : index+1);}
    const start=text.lastIndexOf("\n",offset-1)+1;
    const end=text.indexOf("\n",offset+marker.length);
    const ownLine=!text.slice(start,offset).trim()&&!text.slice(offset+marker.length,end<0?text.length:end).trim();
    return ownLine ? "" : label ? `[Image #${references[index]}]` : `#${references[index]}`;
  });
  return {text:paths.length ? visible.replace(/\n{3,}/g,"\n\n").trim() : text,paths,references};
}
