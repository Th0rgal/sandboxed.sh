/** Interpret image transport markers without changing the stored/model prompt. */
export function messageImages(text: string): {text:string; paths:string[]} {
  const paths:string[]=[];
  const visible=text.replace(/\[Uploaded: ([^\]\r\n]+\.(?:png|jpe?g|webp|gif))\]/gi,(marker,path:string,offset:number)=>{
    // Only paths, never remote URLs or arbitrary prose that resembles a marker.
    if (!/^(?:\/|~\/|\.\.?\/)/.test(path)) return marker;
    const index=paths.push(path);
    const start=text.lastIndexOf("\n",offset-1)+1;
    const end=text.indexOf("\n",offset+marker.length);
    const ownLine=!text.slice(start,offset).trim()&&!text.slice(offset+marker.length,end<0?text.length:end).trim();
    return ownLine?"":`#${index}`;
  });
  if (!paths.length) return {text,paths};
  return {text:visible.replace(/\n{3,}/g,"\n\n").trim(),paths};
}
