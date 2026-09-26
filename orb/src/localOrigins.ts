import {getApiUrl,getJwt,connectionVersion,type Mission} from './api';
import {nativeInvoke} from './clientRuns';
export async function localOrigins():Promise<Mission[]>{
 const invoke=nativeInvoke();if(!invoke||!getJwt())return [];
 const version=connectionVersion();
 try{const rows=await invoke('local_origin_list',{connection:{api_url:getApiUrl(),token:getJwt()}});if(version!==connectionVersion())throw new Error("Connection changed while reading local missions");return Array.isArray(rows)?rows as Mission[]:[];}
 catch(error){if(/unknown command|not found/i.test(String(error)))return [];throw error;}
}
